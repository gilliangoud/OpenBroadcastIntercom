#!/usr/bin/env python3
"""Package DeepFilterNet ONNX exports as a local Core ML model asset.

This maintainer tool prepares the package layout used by the server catalog:

  config.ini
  enc.mlmodelc
  erb_dec.mlmodelc
  df_dec.mlmodelc
  metadata.json

The server can detect this local package as installed immediately. Hosting the
result later only requires publishing the generated archive and adding its URL
and sha256 to intercom-models/manifest.json.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import tarfile
import tempfile
import warnings
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_INPUT = ROOT / "deepfilternet-models" / "DeepFilterNet3_onnx.tar.gz"
DEFAULT_OUTPUT_DIR = ROOT / "deepfilternet-coreml-models"
REQUIRED_PACKAGE_FILES = ["config.ini", "enc.mlmodelc", "erb_dec.mlmodelc", "df_dec.mlmodelc", "metadata.json"]
MODEL_INPUTS = {
    "enc": [
        ("feat_erb", (1, 1, "{sequence_len}", 32)),
        ("feat_spec", (1, 2, "{sequence_len}", 96)),
    ],
    "erb_dec": [
        ("emb", (1, "{sequence_len}", 512)),
        ("e3", (1, 64, "{sequence_len}", 8)),
        ("e2", (1, 64, "{sequence_len}", 8)),
        ("e1", (1, 64, "{sequence_len}", 16)),
        ("e0", (1, 64, "{sequence_len}", 32)),
    ],
    "df_dec": [
        ("emb", (1, "{sequence_len}", 512)),
        ("c0", (1, 64, "{sequence_len}", 96)),
    ],
}


def fail(message: str) -> None:
    raise SystemExit(message)


def safe_extract(archive: Path, destination: Path) -> None:
    with tarfile.open(archive, "r:gz") as handle:
        for member in handle.getmembers():
            target = destination / member.name
            if not target.resolve().is_relative_to(destination.resolve()):
                fail(f"unsafe archive member: {member.name}")
        handle.extractall(destination)


def find_export_dir(root: Path) -> Path:
    for candidate in root.rglob("config.ini"):
        export_dir = candidate.parent
        if all((export_dir / name).exists() for name in ["enc.onnx", "erb_dec.onnx", "df_dec.onnx"]):
            return export_dir
    fail("archive does not contain config.ini plus enc.onnx, erb_dec.onnx, and df_dec.onnx")


def shape_for_sequence(shape: tuple[object, ...], sequence_len: int) -> tuple[int, ...]:
    return tuple(sequence_len if dim == "{sequence_len}" else int(dim) for dim in shape)


def coreml_safe_name(name: str) -> str:
    safe = "".join(char if char.isalnum() or char == "_" else "_" for char in name)
    if not safe or safe[0].isdigit():
        safe = f"output_{safe}"
    return safe


def rewrite_group_linear_einsum(model: "onnx.ModelProto") -> int:
    import onnx
    from onnx import helper

    rewritten = 0
    nodes = []
    for node in model.graph.node:
        if node.op_type == "Einsum":
            attrs = {attr.name: onnx.helper.get_attribute_value(attr) for attr in node.attribute}
            equation = attrs.get("equation")
            if isinstance(equation, bytes):
                equation = equation.decode("utf-8")
            if equation == "btgi,gih->btgh":
                lhs, rhs = node.input
                output = node.output[0]
                safe = f"redline_group_einsum_{rewritten}"
                lhs_expanded = f"{safe}_lhs_unsqueeze"
                rhs_expanded = f"{safe}_rhs_unsqueeze"
                multiplied = f"{safe}_mul"
                nodes.extend(
                    [
                        helper.make_node(
                            "Unsqueeze",
                            [lhs],
                            [lhs_expanded],
                            name=f"{safe}_unsqueeze_lhs",
                            axes=[4],
                        ),
                        helper.make_node(
                            "Unsqueeze",
                            [rhs],
                            [rhs_expanded],
                            name=f"{safe}_unsqueeze_rhs",
                            axes=[0, 1],
                        ),
                        helper.make_node(
                            "Mul",
                            [lhs_expanded, rhs_expanded],
                            [multiplied],
                            name=f"{safe}_mul_node",
                        ),
                        helper.make_node(
                            "ReduceSum",
                            [multiplied],
                            [output],
                            name=f"{safe}_reduce_sum",
                            axes=[3],
                            keepdims=0,
                        ),
                    ]
                )
                rewritten += 1
                continue
        nodes.append(node)
    del model.graph.node[:]
    model.graph.node.extend(nodes)
    return rewritten


def protect_graph_outputs(model: "onnx.ModelProto") -> list[str]:
    from onnx import helper

    names = []
    for output in model.graph.output:
        names.append(output.name)
        protected = f"{output.name}__coreml_output"
        model.graph.node.append(
            helper.make_node(
                "Identity",
                [output.name],
                [protected],
                name=f"{output.name}/CoreMLOutput",
            )
        )
        output.name = protected
    return names


def sanitize_onnx_value_names(model: "onnx.ModelProto") -> None:
    mapping: dict[str, str] = {}

    def rename(name: str) -> str:
        if not name:
            return name
        if name not in mapping:
            mapping[name] = f"redline_value_{len(mapping)}"
        return mapping[name]

    for value in model.graph.input:
        value.name = rename(value.name)
    for value in model.graph.output:
        value.name = rename(value.name)
    for value in model.graph.value_info:
        value.name = rename(value.name)
    for initializer in model.graph.initializer:
        initializer.name = rename(initializer.name)
    for node in model.graph.node:
        for index, name in enumerate(node.input):
            node.input[index] = rename(name)
        for index, name in enumerate(node.output):
            node.output[index] = rename(name)


def convert_onnx_with_coremltools(source: Path, mlpackage: Path, sequence_len: int) -> dict[str, object]:
    try:
        import coremltools as ct
    except ImportError as err:
        raise SystemExit(
            "coremltools is required: python3 -m pip install coremltools on macOS"
        ) from err

    try:
        model = ct.convert(str(source), source="onnx", convert_to="mlprogram")
        model.save(str(mlpackage))
        return {"converter": "coremltools-onnx"}
    except ValueError as err:
        if "source" not in str(err) or "onnx" not in str(err):
            raise

    return convert_onnx_via_pytorch(source, mlpackage, sequence_len)


def convert_onnx_via_pytorch(source: Path, mlpackage: Path, sequence_len: int) -> dict[str, object]:
    try:
        import coremltools as ct
        import onnx
        import torch
        from onnx2pytorch import ConvertModel
        from onnxsim import simplify
    except ImportError as err:
        raise SystemExit(
            "modern coremltools needs bridge dependencies: "
            "python3 -m pip install coremltools onnx onnxsim onnxruntime torch onnx2pytorch"
        ) from err

    model_name = source.stem
    if model_name not in MODEL_INPUTS:
        fail(f"unknown DeepFilterNet ONNX component: {source.name}")

    onnx_model = onnx.load(source)
    rewritten_einsum = rewrite_group_linear_einsum(onnx_model)
    onnx_model, ok = simplify(onnx_model, skip_shape_inference=True)
    if not ok:
        fail(f"onnxsim could not simplify {source.name}")
    output_names = protect_graph_outputs(onnx_model)
    sanitize_onnx_value_names(onnx_model)

    class TupleWrapper(torch.nn.Module):
        def __init__(self, module: torch.nn.Module) -> None:
            super().__init__()
            self.module = module

        def forward(self, *args):  # type: ignore[no-untyped-def]
            outputs = self.module(*args)
            if isinstance(outputs, list):
                return tuple(outputs)
            if isinstance(outputs, tuple):
                return outputs
            return (outputs,)

    torch_model = TupleWrapper(ConvertModel(onnx_model, experimental=True, enable_pruning=False)).eval()
    sample_inputs = tuple(
        torch.zeros(*shape_for_sequence(shape, sequence_len), dtype=torch.float32)
        for _, shape in MODEL_INPUTS[model_name]
    )
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        traced = torch.jit.trace(torch_model, sample_inputs, strict=False)
    inputs = [
        ct.TensorType(name=name, shape=shape_for_sequence(shape, sequence_len))
        for name, shape in MODEL_INPUTS[model_name]
    ]
    output_map = [
        {"name": name, "coreml_name": coreml_safe_name(name)} for name in output_names
    ]
    outputs = [ct.TensorType(name=output["coreml_name"]) for output in output_map]
    model = ct.convert(
        traced,
        inputs=inputs,
        outputs=outputs,
        convert_to="mlprogram",
        minimum_deployment_target=ct.target.macOS12,
    )
    model.save(str(mlpackage))
    return {
        "converter": "onnx-simplifier -> onnx2pytorch -> coremltools",
        "sequence_len": sequence_len,
        "rewritten_group_linear_einsum": rewritten_einsum,
        "inputs": [
            {"name": name, "shape": list(shape_for_sequence(shape, sequence_len))}
            for name, shape in MODEL_INPUTS[model_name]
        ],
        "outputs": output_names,
        "coreml_outputs": output_map,
        "minimum_deployment_target": "macOS12",
    }


def compile_mlpackage(mlpackage: Path, output_dir: Path) -> Path:
    output_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["xcrun", "coremlcompiler", "compile", str(mlpackage), str(output_dir)],
        check=True,
    )
    compiled = output_dir / f"{mlpackage.stem}.mlmodelc"
    if not compiled.exists():
        fail(f"coremlcompiler did not produce {compiled}")
    return compiled


def package_coreml(
    input_archive: Path,
    output_dir: Path,
    package_name: str,
    archive: bool,
    sequence_len: int,
) -> Path:
    if not input_archive.exists():
        fail(f"input archive not found: {input_archive}")
    package_dir = output_dir / package_name
    archive_path = output_dir / f"{package_name}.tar.gz"
    if package_dir.exists():
        shutil.rmtree(package_dir)
    output_dir.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(prefix="redline-dfn-coreml-") as temp_name:
        temp = Path(temp_name)
        safe_extract(input_archive, temp)
        export_dir = find_export_dir(temp)
        work_dir = temp / "coreml"
        work_dir.mkdir()
        package_dir.mkdir()
        shutil.copy2(export_dir / "config.ini", package_dir / "config.ini")

        conversions = {}
        for name in ["enc", "erb_dec", "df_dec"]:
            mlpackage = work_dir / f"{name}.mlpackage"
            conversions[name] = convert_onnx_with_coremltools(
                export_dir / f"{name}.onnx",
                mlpackage,
                sequence_len,
            )
            compiled = compile_mlpackage(mlpackage, work_dir)
            shutil.copytree(compiled, package_dir / f"{name}.mlmodelc")

    metadata = {
        "package": package_name,
        "source_archive": input_archive.name,
        "format": "redline.deepfilternet.coreml.v1",
        "models": ["enc.mlmodelc", "erb_dec.mlmodelc", "df_dec.mlmodelc"],
        "sequence_len": sequence_len,
        "conversions": conversions,
    }
    (package_dir / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
    validate_package(package_dir)

    if archive:
        if archive_path.exists():
            archive_path.unlink()
        with tarfile.open(archive_path, "w:gz") as handle:
            handle.add(package_dir, arcname=package_dir.name)
        return archive_path
    return package_dir


def validate_package(package_dir: Path) -> None:
    missing = [name for name in REQUIRED_PACKAGE_FILES if not (package_dir / name).exists()]
    if missing:
        fail(f"{package_dir} is missing: {', '.join(missing)}")
    print(f"valid Core ML package: {package_dir}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, default=DEFAULT_INPUT, help="DeepFilterNet ONNX .tar.gz archive")
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT_DIR, help="directory for the Core ML package")
    parser.add_argument("--package-name", default="DeepFilterNet3_coreml", help="local package directory/archive name")
    parser.add_argument("--sequence-len", type=int, default=1, help="fixed ONNX sequence length to trace into Core ML")
    parser.add_argument("--archive", action="store_true", help="also create a .tar.gz package for hosting")
    parser.add_argument("--validate", type=Path, help="validate an existing Core ML package directory and exit")
    args = parser.parse_args()

    if args.validate:
        validate_package(args.validate)
        return

    if args.sequence_len < 1:
        fail("--sequence-len must be at least 1")

    result = package_coreml(
        args.input,
        args.output_dir,
        args.package_name,
        args.archive,
        args.sequence_len,
    )
    print(f"wrote: {result}")


if __name__ == "__main__":
    main()
