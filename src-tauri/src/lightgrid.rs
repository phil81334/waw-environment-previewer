//! Bounded BSP31 light-grid sampling for the render-only placed-model preview.
//! Palette interpolation/packing follows the original routines. Bounds-centre
//! sampling and visibility without collision traces are explicit v1 limits.
use crate::BakedScene;
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
struct Entry {
    color: usize,
    primary: u8,
    trace: u8,
}
struct Grid<'a> {
    row_axis: usize,
    col_axis: usize,
    entries: Vec<Entry>,
    colors: &'a [u8],
    points: HashMap<[i32; 3], usize>,
}
#[derive(Clone, Debug)]
struct Sample {
    colors: Vec<usize>,
    weights: Vec<f32>,
    primaries: Vec<u8>,
    traced_corners: usize,
}
fn u16_at(data: &[u8], offset: usize) -> Result<u16, String> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or("Truncated light-grid word")?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}
fn u32_at(data: &[u8], offset: usize) -> Result<u32, String> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or("Truncated light-grid integer")?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}
fn lumps(data: &[u8]) -> Result<HashMap<u32, &[u8]>, String> {
    if data.get(..4) != Some(b"IBSP") || u32_at(data, 4)? != 31 {
        return Err("Light grid requires a WaW BSP31 file".into());
    }
    let count = u32_at(data, 8)? as usize;
    if count > 256 {
        return Err("Invalid light-grid BSP directory count".into());
    }
    let mut cursor = 12 + count * 8;
    let mut result = HashMap::new();
    for i in 0..count {
        let kind = u32_at(data, 12 + i * 8)?;
        let length = u32_at(data, 16 + i * 8)? as usize;
        let end = cursor
            .checked_add(length)
            .ok_or("Light-grid lump size overflow")?;
        let bytes = data
            .get(cursor..end)
            .ok_or("Light-grid BSP lump is truncated")?;
        if result.insert(kind, bytes).is_some() {
            return Err("Duplicate BSP lump in light-grid input".into());
        }
        cursor = end
            .checked_add(3)
            .ok_or("Light-grid lump padding overflow")?
            & !3;
    }
    Ok(result)
}
impl<'a> Grid<'a> {
    fn read(data: &'a [u8]) -> Result<Option<Self>, String> {
        let table = lumps(data)?;
        let header = table.get(&44).copied().unwrap_or(&[]);
        let rows = table.get(&45).copied().unwrap_or(&[]);
        let entries = table.get(&2).copied().unwrap_or(&[]);
        let colors = table.get(&3).copied().unwrap_or(&[]);
        if header.is_empty() && rows.is_empty() && entries.is_empty() && colors.is_empty() {
            return Ok(None);
        }
        if header.len() < 20 || entries.len() % 4 != 0 || colors.len() % 168 != 0 {
            return Err("Malformed light-grid header, entries or directional palette".into());
        }
        if entries.is_empty() || colors.is_empty() {
            return Ok(None);
        }
        let mins = [u16_at(header, 0)?, u16_at(header, 2)?, u16_at(header, 4)?];
        let maxs = [u16_at(header, 6)?, u16_at(header, 8)?, u16_at(header, 10)?];
        let row_axis = u32_at(header, 12)? as usize;
        let col_axis = u32_at(header, 16)? as usize;
        if row_axis > 1 || col_axis > 1 || row_axis == col_axis || (0..3).any(|i| mins[i] > maxs[i])
        {
            return Err("Invalid light-grid axes or bounds".into());
        }
        let row_count = (maxs[row_axis] - mins[row_axis]) as usize + 1;
        if header.len() != 20 + row_count * 2 {
            return Err("Invalid light-grid row-offset table length".into());
        }
        let entries: Vec<_> = entries
            .chunks_exact(4)
            .map(|v| Entry {
                color: u16::from_le_bytes([v[0], v[1]]) as usize,
                primary: v[2],
                trace: v[3],
            })
            .collect();
        if entries.iter().any(|v| v.color >= colors.len() / 168) {
            return Err("Light-grid entry references a missing colour palette".into());
        }
        let mut points = HashMap::new();
        for row in 0..row_count {
            let start = u16_at(header, 20 + row * 2)?;
            if start == u16::MAX {
                continue;
            }
            let mut cursor = start as usize * 4;
            let col_min = u16_at(rows, cursor)? as i32;
            let col_count = u16_at(rows, cursor + 2)? as usize;
            let z_min = u16_at(rows, cursor + 4)? as i32;
            let z_range = u16_at(rows, cursor + 6)?;
            let mut entry = u32_at(rows, cursor + 8)? as usize;
            cursor += 12;
            let mut col = 0usize;
            while col < col_count {
                let run = *rows.get(cursor).ok_or("Truncated light-grid run")? as usize;
                let count = *rows
                    .get(cursor + 1)
                    .ok_or("Truncated light-grid run count")? as usize;
                cursor += 2;
                if run == 0 || run > col_count - col {
                    return Err("Invalid light-grid run length".into());
                }
                let z_start = if count == 0 {
                    0
                } else if z_range > 255 {
                    let value = u16_at(rows, cursor)? as usize;
                    cursor += 2;
                    value
                } else {
                    let value = *rows
                        .get(cursor)
                        .ok_or("Truncated light-grid height offset")?
                        as usize;
                    cursor += 1;
                    value
                };
                let next = entry
                    .checked_add(run * count)
                    .ok_or("Light-grid entry range overflow")?;
                if next > entries.len() || z_start + count > z_range as usize {
                    return Err("Light-grid run references an invalid entry or height range".into());
                }
                for c in 0..run {
                    for z in 0..count {
                        let mut key = [0; 3];
                        key[row_axis] = mins[row_axis] as i32 + row as i32;
                        key[col_axis] = col_min + (col + c) as i32;
                        key[2] = z_min + (z_start + z) as i32;
                        if points.len() >= 4_000_000 {
                            return Err("Light-grid decoded point limit exceeded".into());
                        }
                        if points.insert(key, entry + c * count + z).is_some() {
                            return Err("Duplicate light-grid point".into());
                        }
                    }
                }
                col += run;
                entry = next;
            }
        }
        Ok(Some(Self {
            row_axis,
            col_axis,
            entries,
            colors,
            points,
        }))
    }
    fn sample(&self, point: [f32; 3]) -> Option<Sample> {
        if point.iter().any(|v| !v.is_finite() || v.abs() > 131_000.0) {
            return None;
        }
        let coords = [
            ((point[0].floor() as i32 + 131072) >> 5),
            ((point[1].floor() as i32 + 131072) >> 5),
            ((point[2].floor() as i32 + 131072) >> 6),
        ];
        let fractions = [
            (((point[0] as f64 + 131072.0) * (1.0 / 32.0)) - coords[0] as f64) as f32,
            (((point[1] as f64 + 131072.0) * (1.0 / 32.0)) - coords[1] as f64) as f32,
            (((point[2] as f64 + 131072.0) * (1.0 / 64.0)) - coords[2] as f64) as f32,
        ];
        let mut sample = Sample {
            colors: Vec::new(),
            weights: Vec::new(),
            primaries: Vec::new(),
            traced_corners: 0,
        };
        for row in 0..2 {
            for col in 0..2 {
                for z in 0..2 {
                    let ordinal = row * 4 + col * 2 + z;
                    let mut coord = coords;
                    coord[self.row_axis] += row as i32;
                    coord[self.col_axis] += col as i32;
                    coord[2] += z as i32;
                    let a = if row == 0 {
                        1.0 - fractions[self.row_axis]
                    } else {
                        fractions[self.row_axis]
                    };
                    let b = if col == 0 {
                        1.0 - fractions[self.col_axis]
                    } else {
                        fractions[self.col_axis]
                    };
                    let c = if z == 0 {
                        1.0 - fractions[2]
                    } else {
                        fractions[2]
                    };
                    let weight = a * (b * c);
                    if weight < 0.001 {
                        continue;
                    }
                    if let Some(&index) = self.points.get(&coord) {
                        let entry = self.entries[index];
                        sample.colors.push(entry.color);
                        sample.weights.push(weight);
                        sample.primaries.push(entry.primary);
                        if entry.trace & (1 << ordinal) != 0 {
                            sample.traced_corners += 1;
                        }
                    }
                }
            }
        }
        if sample.weights.is_empty() {
            None
        } else {
            Some(sample)
        }
    }
    fn palette(&self, sample: &Sample) -> [u8; 168] {
        // Original sampler merges repeated colour indices before quantizing.
        let mut colors: Vec<usize> = Vec::new();
        let mut weights: Vec<f32> = Vec::new();
        for (&color, &weight) in sample.colors.iter().zip(&sample.weights) {
            if let Some(index) = colors.iter().position(|&value| value == color) {
                weights[index] += weight;
            } else {
                colors.push(color);
                weights.push(weight);
            }
        }
        let weights = quantize(&weights);
        let mut output = [0; 168];
        for (channel, value) in output.iter_mut().enumerate() {
            let total: i32 = colors
                .iter()
                .zip(&weights)
                .map(|(index, weight)| self.colors[index * 168 + channel] as i32 * weight)
                .sum();
            *value = ((total + 127) >> 8).clamp(0, 255) as u8;
        }
        output
    }
}
// Original 0x681FA0, including first-largest tie handling and sum correction.
fn quantize(weights: &[f32]) -> Vec<i32> {
    let total: f32 = weights.iter().sum();
    let scale = 256.0 * (1.0 / total);
    let mut output: Vec<i32> = weights
        .iter()
        .map(|weight| (weight * scale + 0.5).trunc() as i32)
        .collect();
    let mut largest = 0;
    for i in 1..output.len() {
        if output[i] > output[largest] {
            largest = i;
        }
    }
    let sum: i32 = output.iter().sum();
    output[largest] += 256 - sum;
    output
}
// Original 56 boundary samples, z/y/x order. Eight interiors copy outer corners.
fn volume(palette: &[u8; 168], alpha: u8) -> Vec<u8> {
    let mut output = vec![0; 256];
    let mut sample = 0;
    for z in 0..4 {
        for y in 0..4 {
            for x in 0..4 {
                if x == 0 || x == 3 || y == 0 || y == 3 || z == 0 || z == 3 {
                    let dst = ((z * 4 + y) * 4 + x) * 4;
                    output[dst..dst + 3].copy_from_slice(&palette[sample * 3..sample * 3 + 3]);
                    output[dst + 3] = alpha;
                    sample += 1;
                }
            }
        }
    }
    for z in 1..3 {
        for y in 1..3 {
            for x in 1..3 {
                let src = (((if z == 1 { 0 } else { 3 }) * 4 + if y == 1 { 0 } else { 3 }) * 4
                    + if x == 1 { 0 } else { 3 })
                    * 4;
                let dst = ((z * 4 + y) * 4 + x) * 4;
                let pixel: [u8; 4] = output[src..src + 4].try_into().unwrap();
                output[dst..dst + 4].copy_from_slice(&pixel);
            }
        }
    }
    output
}
fn vector(value: &Value) -> Option<[f32; 3]> {
    let data = value.as_array()?;
    if data.len() != 3 {
        return None;
    }
    let result = [
        data[0].as_f64()? as f32,
        data[1].as_f64()? as f32,
        data[2].as_f64()? as f32,
    ];
    if result.iter().all(|v| v.is_finite()) {
        Some(result)
    } else {
        None
    }
}

pub(crate) fn decorate_models(data: &[u8], scene: &mut BakedScene) -> Result<(), String> {
    let grid = Grid::read(data)?;
    let scene_id = scene.manifest.id.clone();
    let valid_primaries: Vec<usize> = scene
        .manifest
        .lights
        .iter()
        .map(|light| light.index as usize)
        .collect();
    let sun = scene
        .manifest
        .lights
        .iter()
        .find(|light| light.r#type == 1)
        .map(|light| light.index as usize)
        .unwrap_or(0);
    let Some(models) = scene.manifest.models.as_mut() else {
        return Ok(());
    };
    let Some(instances) = models.get_mut("instances").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    let mut covered = 0;
    let mut traced = 0;
    for instance in instances.iter_mut() {
        let id = instance
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let center = vector(&instance["bounds"]["min"])
            .zip(vector(&instance["bounds"]["max"]))
            .map(|(a, b)| {
                [
                    (a[0] + b[0]) * 0.5,
                    (a[1] + b[1]) * 0.5,
                    (a[2] + b[2]) * 0.5,
                ]
            });
        let origin = vector(&instance["origin"]);
        let result = grid.as_ref().and_then(|grid| {
            center
                .and_then(|p| {
                    grid.sample(p)
                        .map(|sample| (p, "worldBoundsCentre", sample))
                })
                .or_else(|| {
                    origin.and_then(|p| {
                        grid.sample(p)
                            .map(|sample| (p, "compiledOriginFallback", sample))
                    })
                })
        });
        let Some((point, source, sample)) = result else {
            instance["lightingProfile"] = json!({"status":"missingGridCoverage"});
            scene.manifest.warnings.push(format!(
                "Model {id}: no compiled light-grid coverage; lit surfaces are skipped."
            ));
            continue;
        };
        let grid = grid.as_ref().unwrap();
        let inherited = instance
            .get("primaryLightIndex")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let mut primary = if inherited != 0 && valid_primaries.contains(&inherited) {
            inherited
        } else {
            0
        };
        if primary == 0 {
            let mut best = 0.0;
            for (&candidate, &weight) in sample.primaries.iter().zip(&sample.weights) {
                let candidate = if candidate == 255 {
                    sun
                } else {
                    candidate as usize
                };
                if candidate != 0 && valid_primaries.contains(&candidate) && weight > best {
                    primary = candidate;
                    best = weight;
                }
            }
        }
        let total: f32 = sample.weights.iter().sum();
        let matching: f32 = sample
            .primaries
            .iter()
            .zip(&sample.weights)
            .filter(|(candidate, _)| {
                **candidate != 0 && **candidate != 255 && **candidate as usize == primary
            })
            .map(|(_, weight)| *weight)
            .sum();
        let visibility = if primary == 0 { 0.0 } else { matching / total };
        let alpha = (visibility * 255.0 + 0.5).trunc().clamp(0.0, 255.0) as u8;
        let buffer = format!("{scene_id}/{id}-lighting.rgba");
        scene
            .buffers
            .insert(buffer.clone(), volume(&grid.palette(&sample), alpha));
        instance["lightingVolumeBuffer"] = json!(buffer);
        instance["primaryLightIndex"] = json!(primary);
        instance["lightingProfile"] = json!({"status":"compiledDirectionalGrid","samplePoint":point,"samplePointSource":source,"validCorners":sample.weights.len(),"traceFlaggedCorners":sample.traced_corners,"visibility":visibility,"collisionTraceCorrections":false,"primaryZeroOr255OcclusionContribution":false});
        covered += 1;
        if sample.traced_corners > 0 {
            traced += 1;
        }
    }
    models["lightingProfile"] = json!({"coveredInstances":covered,"totalInstances":instances.len(),"traceAffectedInstances":traced,"directionalPalette":"original56RgbQuantizedMixAnd4x4x4Expansion","samplePointPolicy":"worldBoundsCentreThenCompiledOrigin","collisionTraces":false});
    if covered > 0 {
        scene.manifest.warnings.push(format!("Model lighting uses compiled directional light-grid colours ({covered} placed models, {traced} with trace-flagged corners). V1 samples world bounds centres, falling back to compiled origins. Collision-based corner rejection and primary 0/255 visibility corrections are not implemented; model lighting near occluders can differ from the game."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quantized_weights_preserve_sum_and_first_largest_tie() {
        assert_eq!(quantize(&[1.0, 1.0, 1.0]), vec![86, 85, 85]);
        assert_eq!(quantize(&[0.01, 0.99]), vec![3, 253]);
    }
    #[test]
    fn directional_volume_preserves_rgb_and_copies_eight_inner_corners() {
        let mut palette = [0; 168];
        for i in 0..56 {
            palette[i * 3] = i as u8;
            palette[i * 3 + 1] = 100 + i as u8;
            palette[i * 3 + 2] = 200 - i as u8;
        }
        let data = volume(&palette, 173);
        assert_eq!(&data[0..4], &[0, 100, 200, 173]);
        assert_eq!(&data[84..88], &[0, 100, 200, 173]);
        assert_eq!(&data[168..172], &[55, 155, 145, 173]);
        assert!(data.chunks_exact(4).all(|pixel| pixel[3] == 173));
    }
    #[test]
    fn malformed_grid_data_does_not_create_fake_ambient() {
        assert!(Grid::read(b"IBSP").is_err());
        let data = [b'I', b'B', b'S', b'P', 31, 0, 0, 0, 0, 0, 0, 0];
        assert!(Grid::read(&data).unwrap().is_none());
    }

    #[test]
    fn grid_range_controls_reject_malformed_runs_and_palette_references() {
        fn fixture(mut rows: Vec<u8>, entry: [u8; 4], axis: u32) -> Vec<u8> {
            let mut header = Vec::new();
            for _ in 0..2 {
                for coordinate in [4096u16, 4096, 2048] {
                    header.extend(coordinate.to_le_bytes());
                }
            }
            header.extend(axis.to_le_bytes());
            header.extend(1u32.to_le_bytes());
            header.extend(0u16.to_le_bytes());
            let mut row = Vec::new();
            for value in [4096u16, 1, 2048, 1] {
                row.extend(value.to_le_bytes());
            }
            row.extend(0u32.to_le_bytes());
            row.append(&mut rows);
            let parts = [
                (2u32, entry.to_vec()),
                (3, vec![128u8; 168]),
                (44, header),
                (45, row),
            ];
            let mut output = b"IBSP".to_vec();
            output.extend(31u32.to_le_bytes());
            output.extend(4u32.to_le_bytes());
            for (kind, bytes) in &parts {
                output.extend(kind.to_le_bytes());
                output.extend((bytes.len() as u32).to_le_bytes());
            }
            for (_, bytes) in parts {
                output.extend(bytes);
                while output.len() % 4 != 0 {
                    output.push(0);
                }
            }
            output
        }
        let valid = fixture(vec![1, 1, 0], [0; 4], 0);
        let grid = Grid::read(&valid).unwrap().unwrap();
        let sample = grid.sample([0.0; 3]).unwrap();
        assert_eq!(sample.weights, vec![1.0]);
        assert_eq!(grid.palette(&sample), [128u8; 168]);
        assert!(grid.sample([1000.0; 3]).is_none());
        assert!(Grid::read(&fixture(vec![0, 1, 0], [0; 4], 0)).is_err());
        assert!(Grid::read(&fixture(vec![2, 1, 0], [0; 4], 0)).is_err());
        assert!(Grid::read(&fixture(vec![1, 2, 0], [0; 4], 0)).is_err());
        assert!(Grid::read(&fixture(vec![1, 1, 1], [0; 4], 0)).is_err());
        assert!(Grid::read(&fixture(vec![1, 1], [0; 4], 0)).is_err());
        assert!(Grid::read(&fixture(vec![1, 1, 0], [1, 0, 0, 0], 0)).is_err());
        assert!(Grid::read(&fixture(vec![1, 1, 0], [0; 4], 1)).is_err());
    }
    // Fresh-attempt original-function evidence is opt-in and never a runtime dependency.
    #[test]
    #[ignore = "requires WAW_GRID_CONTROLS pointing to this attempt's native controls"]
    fn original_native_sampler_rows_and_volume_controls() {
        let root = std::path::PathBuf::from(std::env::var("WAW_GRID_CONTROLS").unwrap());
        let bsp = std::fs::read(std::env::var("WAW_GRID_BSP").unwrap()).unwrap();
        let grid = Grid::read(&bsp).unwrap().unwrap();
        let rows: Value =
            serde_json::from_slice(&std::fs::read(root.join("original-rows.json")).unwrap())
                .unwrap();
        let mut row_comparisons = 0;
        for row in rows.as_array().unwrap() {
            let p = row["point"].as_array().unwrap();
            let point = [
                p[0].as_i64().unwrap() as i32,
                p[1].as_i64().unwrap() as i32,
                p[2].as_i64().unwrap() as i32,
            ];
            for col in 0..2 {
                for z in 0..2 {
                    let mut key = point;
                    key[grid.col_axis] += col;
                    key[2] += z;
                    let actual = grid.points.get(&key).map(|&v| v as i64).unwrap_or(-1);
                    assert_eq!(
                        actual,
                        row["entries"][col as usize * 2 + z as usize]
                            .as_i64()
                            .unwrap(),
                        "row {point:?}"
                    );
                    row_comparisons += 1;
                }
            }
        }
        let text = std::fs::read_to_string(root.join("original-samples.json")).unwrap();
        let samples: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap();
        let mut sample_comparisons = 0;
        for reference in samples.as_array().unwrap() {
            let point = vector(&reference["point"]).unwrap();
            let mut colors = Vec::new();
            let mut weights = Vec::new();
            for i in 0..8 {
                let index = reference["entries"][i].as_i64().unwrap();
                if index >= 0 {
                    colors.push(grid.entries[index as usize].color);
                    weights.push(reference["weights"][i].as_f64().unwrap() as f32);
                }
            }
            let actual = grid.sample(point);
            if colors.is_empty() {
                assert!(actual.is_none());
            } else {
                let actual = actual.unwrap();
                assert_eq!(actual.colors, colors, "palette references at {point:?}");
                assert_eq!(actual.weights.len(), weights.len());
                for (&a, &b) in actual.weights.iter().zip(&weights) {
                    assert!((a - b).abs() < 0.00000001, "weight {a} vs {b} at {point:?}");
                }
            }
            sample_comparisons += 1;
        }
        let packed: Value = serde_json::from_slice(
            &std::fs::read(root.join("../render/volume-reference-results.json")).unwrap(),
        )
        .unwrap();
        let mut palette = [0; 168];
        for i in 0..56 {
            palette[i * 3] = i as u8;
            palette[i * 3 + 1] = 100 + i as u8;
            palette[i * 3 + 2] = 200 - i as u8;
        }
        let bytes = volume(&palette, 173);
        for texel in packed["voxels"].as_array().unwrap() {
            let xyz = texel["xyz"].as_array().unwrap();
            let pos = ((xyz[2].as_u64().unwrap() * 4 + xyz[1].as_u64().unwrap()) * 4
                + xyz[0].as_u64().unwrap()) as usize
                * 4;
            let bgra = texel["BGRA"].as_array().unwrap();
            let rgba = [
                bgra[2].as_u64().unwrap() as u8,
                bgra[1].as_u64().unwrap() as u8,
                bgra[0].as_u64().unwrap() as u8,
                bgra[3].as_u64().unwrap() as u8,
            ];
            assert_eq!(&bytes[pos..pos + 4], &rgba);
        }
        println!("PASS {row_comparisons} original row references, {sample_comparisons} original no-trace sampler queries, 64 original packed directional texels");
    }
}
