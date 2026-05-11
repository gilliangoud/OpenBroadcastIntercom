#!/usr/bin/env swift

import CoreML
import Foundation

struct PackageMetadata: Decodable {
    struct Conversion: Decodable {
        struct Input: Decodable {
            let name: String
            let shape: [Int]
        }

        struct Output: Decodable {
            let name: String
            let coreml_name: String
        }

        let inputs: [Input]
        let coreml_outputs: [Output]?
    }

    let package: String
    let conversions: [String: Conversion]
}

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(1)
}

let arguments = CommandLine.arguments
guard arguments.count == 2 else {
    fail("usage: verify-deepfilternet-coreml.swift <DeepFilterNet Core ML package directory>")
}

let packageURL = URL(fileURLWithPath: arguments[1], isDirectory: true)
let metadataURL = packageURL.appendingPathComponent("metadata.json")
let metadataData = try Data(contentsOf: metadataURL)
let metadata = try JSONDecoder().decode(PackageMetadata.self, from: metadataData)

let config = MLModelConfiguration()
config.computeUnits = .all

for component in ["enc", "erb_dec", "df_dec"] {
    guard let conversion = metadata.conversions[component] else {
        fail("metadata is missing conversion info for \(component)")
    }

    let modelURL = packageURL.appendingPathComponent("\(component).mlmodelc", isDirectory: true)
    let model = try MLModel(contentsOf: modelURL, configuration: config)
    var features: [String: MLFeatureValue] = [:]

    for input in conversion.inputs {
        let shape = input.shape.map { NSNumber(value: $0) }
        let array = try MLMultiArray(shape: shape, dataType: .float32)
        for index in 0..<array.count {
            array[index] = 0
        }
        features[input.name] = MLFeatureValue(multiArray: array)
    }

    let provider = try MLDictionaryFeatureProvider(dictionary: features)
    let prediction = try model.prediction(from: provider)
    let actual = prediction.featureNames.sorted()
    let expected = (conversion.coreml_outputs ?? []).map(\.coreml_name).sorted()

    if !expected.isEmpty && actual != expected {
        fail("\(component) output mismatch. expected \(expected), got \(actual)")
    }

    print("\(component): ok inputs=\(conversion.inputs.map(\.name).joined(separator: ",")) outputs=\(actual.joined(separator: ","))")
}

print("valid Core ML runtime package: \(metadata.package)")
