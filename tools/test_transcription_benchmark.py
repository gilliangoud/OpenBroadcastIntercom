import json
import struct
import sys
import tempfile
import unittest
import wave
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run_transcription_benchmarks as suite
import run_mlx_whisper_benchmarks as mlx_suite
import run_moonshine_benchmarks as moonshine_suite
import run_parakeet_benchmarks as parakeet_suite
import run_whisperkit_benchmarks as whisperkit_suite
import moonshine_benchmark
import parakeet_benchmark
import transcription_benchmark as tb
import whisperkit_benchmark


def write_mono_wav(path: Path, *, sample_rate: int = 16_000, samples: int = 1600) -> None:
    frames = struct.pack("<" + "h" * samples, *([0] * samples))
    with wave.open(str(path), "wb") as handle:
        handle.setnchannels(1)
        handle.setsampwidth(2)
        handle.setframerate(sample_rate)
        handle.writeframes(frames)


class TranscriptionBenchmarkTests(unittest.TestCase):
    def test_text_scoring_normalizes_case_and_punctuation(self):
        score = tb.score_text("Check, one two!", "check one too")
        self.assertEqual(score["reference_words"], 3)
        self.assertEqual(score["word_errors"], 1)
        self.assertAlmostEqual(score["wer"], 1 / 3)

    def test_load_corpus_rejects_duplicate_ids(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            wav = root / "clip.wav"
            write_mono_wav(wav)
            corpus = root / "corpus.json"
            corpus.write_text(
                json.dumps(
                    {
                        "version": 1,
                        "segments": [
                            {"id": "same", "audio": "clip.wav", "expected_text": "check one"},
                            {"id": "same", "audio": "clip.wav", "expected_text": "check two"},
                        ],
                    }
                ),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(tb.BenchmarkError, "duplicate segment id"):
                tb.load_corpus(corpus)

    def test_scores_prediction_file_and_renders_markdown(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            audio_dir = root / "audio"
            audio_dir.mkdir()
            wav = audio_dir / "fixture.wav"
            write_mono_wav(wav, samples=3200)
            corpus_path = root / "corpus.json"
            corpus_path.write_text(
                json.dumps(
                    {
                        "version": 1,
                        "name": "fixture corpus",
                        "segments": [
                            {
                                "id": "fixture-001",
                                "audio": "audio/fixture.wav",
                                "expected_text": "Check one two",
                                "device": {"kind": "online_fixture", "name": "LibriSpeech dummy"},
                                "noise": {"kind": "clean"},
                                "cleanup": {"pipeline": "none"},
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )
            corpus = tb.load_corpus(corpus_path)
            result = tb.score_corpus(
                corpus,
                {"fixture-001": {"text": "check one too", "latency_ms": 50}},
                model_id="fixture-model",
                runtime="fixture",
            )
            self.assertEqual(result["summary"]["segments"], 1)
            self.assertAlmostEqual(result["summary"]["wer"], 1 / 3)
            self.assertAlmostEqual(result["summary"]["average_realtime_factor"], 0.25)
            markdown = tb.render_markdown(result)
            self.assertIn("Transcription Benchmark: fixture-model", markdown)
            self.assertIn("LibriSpeech dummy", markdown)
            self.assertIn("33.33%", markdown)

    def test_missing_prediction_is_an_error(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            wav = root / "clip.wav"
            write_mono_wav(wav)
            corpus_path = root / "corpus.json"
            corpus_path.write_text(
                json.dumps(
                    {
                        "version": 1,
                        "segments": [
                            {"id": "clip", "audio": "clip.wav", "expected_text": "check one"}
                        ],
                    }
                ),
                encoding="utf-8",
            )
            corpus = tb.load_corpus(corpus_path)
            with self.assertRaisesRegex(tb.BenchmarkError, "missing predictions"):
                tb.score_corpus(corpus, {}, model_id="fixture-model")

    def test_load_predictions_accepts_direct_segment_map(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "predictions.json"
            path.write_text(
                json.dumps({"clip-001": {"text": "check one", "latency_ms": 12.5}}),
                encoding="utf-8",
            )
            model_id, predictions = tb.load_predictions(path)
            self.assertIsNone(model_id)
            self.assertEqual(predictions["clip-001"]["text"], "check one")

    def test_build_corpus_from_recording_session(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            session = root / "session-123"
            session.mkdir()
            write_mono_wav(session / "user-1.wav", sample_rate=48_000, samples=4800)
            (session / "metadata.jsonl").write_text(
                "\n".join(
                    [
                        json.dumps(
                            {
                                "kind": "ingest_frame",
                                "timestamp_ms": 1000,
                                "session_id": "session-123",
                                "frame_index": 0,
                                "user_id": 1,
                                "user_name": "Operator",
                                "target": {"kind": "channel", "id": 7},
                                "codec": "opus",
                                "talk_mode": "ptt",
                                "peak": 0.5,
                                "rms": 0.1,
                            }
                        ),
                        json.dumps(
                            {
                                "kind": "ingest_frame",
                                "timestamp_ms": 1020,
                                "session_id": "session-123",
                                "frame_index": 1,
                                "user_id": 1,
                                "user_name": "Operator",
                                "target": {"kind": "channel", "id": 7},
                                "codec": "opus",
                                "talk_mode": "ptt",
                                "peak": 0.7,
                                "rms": 0.3,
                            }
                        ),
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            out = root / "corpus.json"
            corpus_data = tb.build_corpus_from_recording(
                session,
                transcripts={"user-1": "Check one two"},
                output_path=out,
                name=None,
                device_kind="mobile",
                device_name="iPhone 13",
                noise_kind="venue",
                cleanup_pipeline="deepfilternet+normalization",
                mode="reliable",
                chunk_ms=18000,
                overlap_ms=1600,
                prompt="Sports officiating intercom.",
            )
            out.write_text(json.dumps(corpus_data), encoding="utf-8")
            corpus = tb.load_corpus(out)
            self.assertEqual(corpus.name, "session-123")
            self.assertEqual(corpus.segments[0].id, "session-123-user-1")
            metadata = corpus.segments[0].metadata
            self.assertEqual(metadata["device"]["name"], "iPhone 13")
            self.assertEqual(metadata["codec"], "opus")
            self.assertEqual(metadata["source"]["frames"], 2)
            self.assertEqual(metadata["gain"]["peak_linear"], 0.7)
            self.assertAlmostEqual(metadata["gain"]["rms_linear"], 0.2)

    def test_parse_partial_macos_time_output(self):
        metrics = suite.parse_macos_time_l(
            "       14.57 real        43.56 user         1.80 sys\n"
            "time: sysctl kern.clockrate: Operation not permitted\n"
        )
        self.assertAlmostEqual(metrics["user_time_ms"], 43560.0)
        self.assertAlmostEqual(metrics["system_time_ms"], 1800.0)
        self.assertAlmostEqual(metrics["child_cpu_time_ms"], 45360.0)

    def test_mlx_runner_keeps_venv_python_symlink(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            base_python = root / "homebrew-python"
            base_python.write_text("", encoding="utf-8")
            venv_bin = root / "venv" / "bin"
            venv_bin.mkdir(parents=True)
            venv_python = venv_bin / "python"
            venv_python.symlink_to(base_python)

            absolute = mlx_suite.absolute_without_symlink_resolution(venv_python)

            self.assertEqual(absolute, venv_python.absolute())
            self.assertNotEqual(absolute, base_python.resolve())

    def test_whisperkit_extracts_json_text(self):
        output = json.dumps({"segments": [{"text": "Check one."}, {"text": "Clock stopped."}]})

        text = whisperkit_benchmark.extract_transcript(output, "")

        self.assertEqual(text, "Check one. Clock stopped.")

    def test_whisperkit_extracts_prefixed_text_line(self):
        output = "Loading model\nTranscription: Ref one check\n"

        text = whisperkit_benchmark.extract_transcript(output, "")

        self.assertEqual(text, "Ref one check")

    def test_whisperkit_model_spec_supports_prefix_and_path(self):
        prefixed = whisperkit_suite.parse_model_spec("wk-distil=distil:large-v3")
        local_path = whisperkit_suite.parse_model_spec("wk-local=path:/tmp/model")

        self.assertEqual(prefixed["id"], "wk-distil")
        self.assertEqual(prefixed["prefix"], "distil")
        self.assertEqual(prefixed["model"], "large-v3")
        self.assertEqual(local_path["path"], "/tmp/model")

    def test_parakeet_output_text_accepts_object_text(self):
        class Hypothesis:
            text = "Ref one check."

        self.assertEqual(parakeet_benchmark.output_text(Hypothesis()), "Ref one check.")
        self.assertEqual(parakeet_benchmark.output_text({"pred_text": "Clock stopped."}), "Clock stopped.")

    def test_parakeet_runner_keeps_venv_python_symlink(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            base_python = root / "homebrew-python"
            base_python.write_text("", encoding="utf-8")
            venv_bin = root / "venv" / "bin"
            venv_bin.mkdir(parents=True)
            venv_python = venv_bin / "python"
            venv_python.symlink_to(base_python)

            absolute = parakeet_suite.absolute_without_symlink_resolution(venv_python)

            self.assertEqual(absolute, venv_python.absolute())
            self.assertNotEqual(absolute, base_python.resolve())

    def test_moonshine_normalizes_transcribe_output(self):
        self.assertEqual(
            moonshine_benchmark.normalize_transcribe_output(["Ref one", {"text": "check"}]),
            "Ref one check",
        )

    def test_moonshine_runner_keeps_venv_python_symlink(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            base_python = root / "homebrew-python"
            base_python.write_text("", encoding="utf-8")
            venv_bin = root / "venv" / "bin"
            venv_bin.mkdir(parents=True)
            venv_python = venv_bin / "python"
            venv_python.symlink_to(base_python)

            absolute = moonshine_suite.absolute_without_symlink_resolution(venv_python)

            self.assertEqual(absolute, venv_python.absolute())
            self.assertNotEqual(absolute, base_python.resolve())


if __name__ == "__main__":
    unittest.main()
