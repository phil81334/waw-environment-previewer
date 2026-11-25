//! Validate the camera axes used for FX culling, fades and billboard orientation.

use serde_json::Value;

pub(crate) type Axis = [[f32; 3]; 3];

/// Engine camera axes are forward, left and up. Screen right is negative left.
pub(crate) const DEFAULT_AXIS: Axis = [[-1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0]];

/// Camera axes are internal preview settings, not authored emitter rotations.
/// Reject a malformed basis before it can affect culling, fades or billboards.
pub(crate) fn parse_axis(value: Option<&Value>) -> Result<Axis, String> {
    let Some(value) = value else {
        return Ok(DEFAULT_AXIS);
    };
    let rows = value
        .as_array()
        .ok_or("camera axis must contain three vectors")?;
    if rows.len() != 3 {
        return Err("camera axis must contain three vectors".into());
    }
    let axis = [point(&rows[0])?, point(&rows[1])?, point(&rows[2])?];
    validate_axis(axis)?;
    Ok(axis)
}

fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn validate_axis(axis: Axis) -> Result<(), String> {
    if axis
        .iter()
        .flatten()
        .any(|component| !component.is_finite())
    {
        return Err("camera axis must be finite".into());
    }
    for row in 0..3 {
        if (dot(axis[row], axis[row]) - 1.0).abs() > 0.0001 {
            return Err("camera axis vectors must have unit length".into());
        }
        for other in row + 1..3 {
            if dot(axis[row], axis[other]).abs() > 0.0001 {
                return Err("camera axis vectors must be perpendicular".into());
            }
        }
    }
    let forward_cross_left = [
        axis[0][1] * axis[1][2] - axis[0][2] * axis[1][1],
        axis[0][2] * axis[1][0] - axis[0][0] * axis[1][2],
        axis[0][0] * axis[1][1] - axis[0][1] * axis[1][0],
    ];
    if dot(forward_cross_left, axis[2]) < 0.9999 {
        return Err("camera axis must use the forward/left/up handedness".into());
    }
    Ok(())
}

fn point(value: &Value) -> Result<[f32; 3], String> {
    let coordinates = value.as_array().ok_or("camera position must be an array")?;
    if coordinates.len() != 3 {
        return Err("camera position must contain three coordinates".into());
    }

    let mut output = [0.0; 3];
    for axis in 0..3 {
        output[axis] = coordinates[axis]
            .as_f64()
            .ok_or("camera coordinate must be numeric")? as f32;
        if !output[axis].is_finite() {
            return Err("camera coordinate is not finite".into());
        }
    }

    Ok(output)
}
