//! Find a nearby view of an authored light inside its part of the compiled world.
use waw_environment_previewer::BakedScene;

type Point = [f32; 3];

fn sub(a: Point, b: Point) -> Point {
    std::array::from_fn(|i| a[i] - b[i])
}

fn dot(a: Point, b: Point) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: Point, b: Point) -> Point {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

// Two-sided Möller–Trumbore intersection. A ceiling or the back of a brush
// must stop the camera regardless of the winding used for rendering.
fn hit_distance(origin: Point, direction: Point, [a, b, c]: &[Point; 3]) -> Option<f32> {
    let ab = sub(*b, *a);
    let ac = sub(*c, *a);
    let p = cross(direction, ac);
    let determinant = dot(ab, p);
    if determinant.abs() < 0.00001 {
        return None;
    }
    let from_a = sub(origin, *a);
    let u = dot(from_a, p) / determinant;
    let q = cross(from_a, ab);
    let v = dot(direction, q) / determinant;
    if !(0.0..=1.0).contains(&u) || v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = dot(ac, q) / determinant;
    (t > 0.001).then_some(t)
}

pub fn eye(scene: &BakedScene, origin: Point, reference: Point) -> Result<Point, String> {
    if origin.iter().chain(&reference).any(|v| !v.is_finite()) {
        return Err("Invalid light or camera position.".into());
    }
    let geometry = &scene.manifest.geometry;
    let vertices = scene
        .buffers
        .get(&geometry.vertex_buffer)
        .ok_or("Missing world vertices.")?;
    let indices = scene
        .buffers
        .get(&geometry.index_buffer)
        .ok_or("Missing world indices.")?;
    if geometry.stride != 44 || geometry.index_type != "uint32" || indices.len() % 12 != 0 {
        return Err("Cannot frame a light with this world mesh.".into());
    }
    let mut triangles = Vec::with_capacity(indices.len() / 12);
    for face in indices.chunks_exact(12) {
        let mut triangle = [[0.0; 3]; 3];
        for (point, bytes) in triangle.iter_mut().zip(face.chunks_exact(4)) {
            let index = u32::from_le_bytes(bytes.try_into().unwrap()) as usize;
            let offset = index
                .checked_mul(geometry.stride)
                .ok_or("Invalid world index.")?;
            let xyz = vertices
                .get(offset..)
                .and_then(|tail| tail.get(..12))
                .ok_or("Missing world vertex.")?;
            for (axis, component) in xyz.chunks_exact(4).enumerate() {
                point[axis] = f32::from_le_bytes(component.try_into().unwrap());
            }
        }
        triangles.push(triangle);
    }

    // Approach from the scene's reference camera side, close to the light.
    // Try other bearings if a wall blocks that view. The light radius controls
    // illumination and has no bearing on how much space is available to stand in.
    const STANDOFF: f32 = 64.0;
    const CLEARANCE: f32 = 8.0;
    let toward = sub(reference, origin);
    let yaw = toward[1].atan2(toward[0]);
    let slope = (toward[2] / STANDOFF).clamp(-0.75, 0.75);
    let mut best = None;
    let mut best_distance = CLEARANCE;
    for elevation in [slope, 0.0, -0.65, 0.65] {
        for turn in [0.0_f32, 45.0, -45.0, 90.0, -90.0, 135.0, -135.0, 180.0] {
            let angle = yaw + turn.to_radians();
            let length = (1.0 + elevation * elevation).sqrt();
            let direction = [
                angle.cos() / length,
                angle.sin() / length,
                elevation / length,
            ];
            let distance = triangles
                .iter()
                .filter_map(|triangle| hit_distance(origin, direction, triangle))
                .fold(STANDOFF + CLEARANCE, f32::min)
                - CLEARANCE;
            if distance <= best_distance {
                continue;
            }
            let eye = std::array::from_fn(|i| origin[i] + distance * direction[i]);
            // Brush surfaces are already in the triangle mesh. A trigger's
            // bounds can cover an entire room without enclosing any solid geometry.
            if scene.manifest.objects.iter().flatten().any(|object| {
                object.kind != "brush"
                    && object.draw_count > 0
                    && (0..3)
                        .all(|i| eye[i] >= object.bounds.min[i] && eye[i] <= object.bounds.max[i])
            }) {
                continue;
            }
            if distance >= STANDOFF {
                return Ok(eye);
            }
            best_distance = distance;
            best = Some(eye);
        }
    }
    best.ok_or_else(|| "There is no clear camera position near this light.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walls_block_both_directions_but_not_rays_pointing_away() {
        let wall = [[-20., -20., 12.], [20., -20., 12.], [0., 20., 12.]];
        assert_eq!(hit_distance([0.; 3], [0., 0., 1.], &wall), Some(12.));
        assert_eq!(hit_distance([0., 0., 20.], [0., 0., -1.], &wall), Some(8.));
        assert_eq!(hit_distance([0.; 3], [0., 0., -1.], &wall), None);
    }
}
