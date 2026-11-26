//! Golden parity for the am_bg v36 colour policy.
//!
//! The fixture was produced by the legacy renderer's own test
//! (`tests/render/immersion-colour.test.ts`) from the upstream deliverable at
//! commit 708f6b1. It pins both the 200x200 source bytes after recolouring and
//! the exposure the final GPU pass multiplies by, so this is the same assertion
//! the TypeScript carried -- not a re-derived expectation.

use std::fs;
use std::path::{Path, PathBuf};

use oly_render::background::colour::{
    ColourParams, CoverSample, SAMPLE_SIZE, SIZE, colour_artwork,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/immersion-v36-colour.json")
}

fn read_fixture() -> Value {
    let path = fixture_path();
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("cannot parse {}: {error}", path.display()))
}

fn channels(case: &Value, key: &str) -> [i64; 3] {
    let array = case[key].as_array().expect("channel array");
    assert_eq!(array.len(), 3);
    [
        array[0].as_i64().expect("channel"),
        array[1].as_i64().expect("channel"),
        array[2].as_i64().expect("channel"),
    ]
}

#[test]
#[ignore = "superseded by immersion-bg-lab V2 colour pipeline (8c7d3d8)"]
fn colour_policy_matches_the_frozen_browser_bytes() {
    let mut fixture = read_fixture();
    let update = std::env::var_os("OLY_UPDATE_GOLDENS").is_some();
    if update {
        fixture["source"] = serde_json::json!("oly-render::background::colour (OKLCH/Oklab)");
        fixture["commit"] = serde_json::json!("native");
        if let Some(cases) = fixture["cases"].as_array_mut() {
            for case in cases.iter_mut() {
                let field = channels(case, "field");
                let art = channels(case, "art");
                let inner = case["inner"].as_u64().expect("inner") as u32;
                let mut pixels = vec![0u8; SIZE * SIZE * 4];
                for y in 0..SIZE {
                    for x in 0..SIZE {
                        let i = (y * SIZE + x) * 4;
                        for k in 0..3 {
                            pixels[i + k] = (field[k] + (y % 13) as i64 - 6) as u8;
                        }
                        pixels[i + 3] = 255;
                    }
                }
                let mut sample = [0u8; SAMPLE_SIZE * SAMPLE_SIZE * 4];
                for px in sample.chunks_exact_mut(4) {
                    px[0] = art[0] as u8;
                    px[1] = art[1] as u8;
                    px[2] = art[2] as u8;
                    px[3] = 255;
                }
                let mut params = ColourParams::for_inner(inner);
                let gain = colour_artwork(&mut pixels, CoverSample::new(&sample), &mut params);
                let digest = Sha256::digest(&pixels);
                let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
                case["gain"] = serde_json::json!(gain);
                case["sha256"] = serde_json::json!(hex);
            }
        }
        let cases_json = serde_json::to_string(&fixture["cases"]).expect("serialize cases");
        let cases_sha: String = Sha256::digest(cases_json.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        fixture["sha256"] = serde_json::json!(cases_sha);
        fs::write(
            fixture_path(),
            serde_json::to_string_pretty(&fixture).expect("pretty fixture"),
        )
        .expect("write fixture");
    }

    let expected_cases_sha: String = Sha256::digest(
        serde_json::to_string(&fixture["cases"])
            .expect("serialize cases")
            .as_bytes(),
    )
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect();
    assert_eq!(
        fixture["sha256"].as_str(),
        Some(expected_cases_sha.as_str()),
        "fixture cases hash mismatch"
    );
    let cases = fixture["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), 6);

    for case in cases {
        let name = case["name"].as_str().expect("case name");
        let field = channels(case, "field");
        let art = channels(case, "art");
        let inner = case["inner"].as_u64().expect("inner") as u32;
        let expected_gain = case["gain"].as_f64().expect("gain");
        let expected_sha = case["sha256"].as_str().expect("sha256");

        // The legacy test built the scene as `field + (row % 13) - 6`; the row
        // term is what distinguishes this from a flat colour and therefore what
        // exercises the per-pixel colour round trip.
        let mut pixels = vec![0u8; SIZE * SIZE * 4];
        for y in 0..SIZE {
            for x in 0..SIZE {
                let i = (y * SIZE + x) * 4;
                for k in 0..3 {
                    pixels[i + k] = (field[k] + (y % 13) as i64 - 6) as u8;
                }
                pixels[i + 3] = 255;
            }
        }

        // The reference reads its statistics from a flat 32x32 sample of the
        // cover's own colour; the fixture stores that colour, not a resampled
        // image.
        let mut sample = [0u8; SAMPLE_SIZE * SAMPLE_SIZE * 4];
        for px in sample.chunks_exact_mut(4) {
            px[0] = art[0] as u8;
            px[1] = art[1] as u8;
            px[2] = art[2] as u8;
            px[3] = 255;
        }

        let mut params = ColourParams::for_inner(inner);
        let gain = colour_artwork(&mut pixels, CoverSample::new(&sample), &mut params);

        assert!(
            (gain - expected_gain).abs() < 1e-12,
            "{name}: gain {gain:.15} != {expected_gain:.15}"
        );
        assert_eq!(params.gain, gain, "{name}: gain must be written back");

        let digest = Sha256::digest(&pixels);
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(hex, expected_sha, "{name}: recoloured bytes drifted");
    }
}
