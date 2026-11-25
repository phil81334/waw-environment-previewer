//! A bounded draw-state snapshot, independent of the advancing scheduler.
//!
//! Camera movement, including while paused, rebuilds billboards, distance fades,
//! trails and draw order from this state. It never advances births or consumes
//! the simulation's random stream merely to update the preview camera.
use super::*;

pub(crate) struct DrawSnapshot {
    now: u32,
    definitions: std::sync::Arc<BTreeMap<String, Vec<Definition>>>,
    instances: Vec<Instance>,
    particles: Vec<Particle>,
    order: Vec<usize>,
    trail: Option<crate::fx::trail_state::State>,
    model_bounds: Value,
    name: Value,
    packed_lighting: Value,
}

impl DrawSnapshot {
    pub(super) fn capture(
        now: u32,
        definitions: std::sync::Arc<BTreeMap<String, Vec<Definition>>>,
        instances: &[Instance],
        particles: &[Particle],
        order: &[usize],
        trail: Option<&crate::fx::trail_state::State>,
        source: &Value,
    ) -> Self {
        let mut model_bounds = serde_json::Map::new();
        if let Some(models) = source["modelInfo"].as_object() {
            for (name, asset) in models {
                model_bounds.insert(name.clone(), asset["bounds"].clone());
            }
        }
        Self {
            now,
            definitions,
            instances: instances.to_vec(),
            particles: particles.to_vec(),
            order: order.to_vec(),
            trail: trail.cloned(),
            model_bounds: Value::Object(model_bounds),
            name: source["name"].clone(),
            packed_lighting: source["previewPackedLighting"].clone(),
        }
    }

    pub(crate) fn render(
        &self,
        camera: [f32; 3],
        camera_axis: crate::fx::camera::Axis,
    ) -> Result<Value, String> {
        if !camera
            .iter()
            .chain(camera_axis.iter().flatten())
            .all(|value| value.is_finite())
        {
            return Err("Invalid scene effect draw camera.".into());
        }
        let mut order = self.order.clone();
        // Keep authored definition priority and stable equal-distance order.
        order.sort_by(|a, b| {
            distance_squared(self.instances[*b].origin, camera)
                .total_cmp(&distance_squared(self.instances[*a].origin, camera))
        });
        let mut draws = Vec::new();
        let mut lights = Vec::new();
        let mut models = Vec::new();
        let mut selected_spot = None;
        let mut cloud_count = 0;
        for instance_index in order.iter().copied() {
            let instance = &self.instances[instance_index];
            let layers = &self.definitions[&instance.name];
            for visual_class in [0, 1, 2] {
                let mut particles = self
                    .particles
                    .iter()
                    .filter(|particle| {
                        particle.effect == instance_index
                            && match layers[particle.layer].kind.as_str() {
                                "model" | "light" | "spotLight" => visual_class == 1,
                                "cloud" => visual_class == 2,
                                _ => visual_class == 0,
                            }
                    })
                    .collect::<Vec<_>>();
                if visual_class == 0 {
                    particles.sort_by(|a, b| {
                        layers[a.layer]
                            .sort
                            .cmp(&layers[b.layer].sort)
                            .then_with(|| {
                                particle_sort_distance(
                                    particle_position(
                                        &layers[b.layer],
                                        b,
                                        (self.now as i64 - b.birth).max(0) as f32,
                                    ),
                                    camera,
                                )
                                .total_cmp(
                                    &particle_sort_distance(
                                        particle_position(
                                            &layers[a.layer],
                                            a,
                                            (self.now as i64 - a.birth).max(0) as f32,
                                        ),
                                        camera,
                                    ),
                                )
                            })
                    });
                }
                for particle in particles {
                    let definition = &layers[particle.layer];
                    if (self.now as i64) < particle.birth {
                        continue;
                    }
                    if definition.kind == "model" {
                        if let Some(mut model) = model_particle(definition, particle, self.now) {
                            model["effect"] = json!(particle.effect);
                            model["effectName"] = json!(instance.name);
                            model["bounds"] =
                                self.model_bounds[model["model"].as_str().unwrap()].clone();
                            models.push(model);
                        }
                        continue;
                    }
                    if matches!(definition.kind.as_str(), "light" | "spotLight") {
                        if lights.len() < 32 {
                            if let Some(mut light) =
                                light_particle(definition, particle, self.now, camera)
                            {
                                light["effect"] = json!(particle.effect);
                                if definition.kind == "spotLight" {
                                    selected_spot = Some(light);
                                } else {
                                    lights.push(light);
                                }
                            }
                        }
                        continue;
                    }
                    if definition.materials.is_empty() {
                        continue;
                    }
                    let (draw, _, _) =
                        draw_particle(definition, particle, self.now, camera, camera_axis)?;
                    if draw.is_null() {
                        continue;
                    }
                    if definition.kind == "cloud" {
                        cloud_count += 1;
                        if cloud_count > 256 {
                            return Err("Native scene cloud limit of 256 exceeded.".into());
                        }
                    }
                    draws.push(draw);
                }
            }
        }
        if lights.len() < 32 {
            if let Some(light) = selected_spot {
                lights.push(light);
            }
        }
        if let Some(trail) = &self.trail {
            let frame = trail.draw(self.now, camera)?;
            draws.extend(
                frame["draws"]
                    .as_array()
                    .ok_or("Missing scene trail draws.")?
                    .iter()
                    .cloned(),
            );
        }
        Ok(
            json!({"name":self.name,"time":self.now,"draws":draws,"models":models,
            "lights":lights,"previewPackedLighting":self.packed_lighting}),
        )
    }
}
