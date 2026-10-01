//! Implicit n=1 stream-power incision, mass-conserving sediment routing, and
//! conservative hillslope transport. Units are metres, years and kilograms.
//! The implicit downstream-first integration follows Braun & Willett (2013),
//! doi:10.1016/j.geomorph.2012.10.008. Sediment capacity is a reduced closure,
//! not an implementation of the full FastScape transport/deposition equation.

use crate::{
    geometry::neighbours,
    hydrology::{self, YEAR_SECONDS},
    Cell, Material, WorldConfig,
};
use serde::{Deserialize, Serialize};

const SEDIMENT_DENSITY: f64 = 1700.0;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ErosionReport {
    pub steps: u32,
    pub simulated_years: f64,
    pub bedrock_eroded_kg: f64,
    pub sediment_stored_kg: f64,
    pub sediment_exported_kg: f64,
    pub marine_deposited_kg: f64,
    pub uplift_volume_m3: f64,
    pub mass_balance_relative_error: f64,
    pub max_step_mass_balance_relative_error: f64,
    pub rms_height_change_m: f64,
    pub max_height_change_m: f64,
    pub incised_cells: u64,
    pub breached_depressions: u64,
}

pub(crate) fn rock_density(material: Material) -> f64 {
    match material {
        Material::Basalt => 3000.0,
        Material::Granite => 2700.0,
        Material::Sandstone => 2450.0,
        Material::Limestone => 2600.0,
        Material::Alluvium => SEDIMENT_DENSITY,
        Material::Ice => 917.0,
    }
}

fn height(i: usize, cells: &[Cell], bedrock: &[f64], sediment: &[f64]) -> f64 {
    bedrock[i] + sediment[i] / (cells[i].area_m2 * SEDIMENT_DENSITY)
}

fn remove(
    i: usize,
    depth: f64,
    cells: &[Cell],
    bedrock: &mut [f64],
    sediment: &mut [f64],
    report: &mut ErosionReport,
) -> f64 {
    if depth <= 0.0 {
        return 0.0;
    }
    let available = sediment[i] / (cells[i].area_m2 * SEDIMENT_DENSITY);
    let soil_depth = depth.min(available);
    let soil_mass = soil_depth * cells[i].area_m2 * SEDIMENT_DENSITY;
    sediment[i] = (sediment[i] - soil_mass).max(0.0);
    let rock_depth = depth - soil_depth;
    let rock_mass = rock_depth * cells[i].area_m2 * rock_density(cells[i].material);
    bedrock[i] -= rock_depth;
    report.bedrock_eroded_kg += rock_mass;
    soil_mass + rock_mass
}

fn write_heights(cells: &mut [Cell], bedrock: &[f64], sediment: &[f64]) {
    for (i, c) in cells.iter_mut().enumerate() {
        c.bedrock_m = bedrock[i] as f32;
        c.sediment_m = (sediment[i] / (c.area_m2 * SEDIMENT_DENSITY)) as f32;
        c.height_m = (bedrock[i] + f64::from(c.sediment_m)) as f32;
    }
}

/// Exact backward-Euler solution for dz/dt = -c (z-z_receiver), n=1.
pub fn implicit_height(old: f64, downstream: f64, coefficient: f64, dt_years: f64) -> f64 {
    if old <= downstream {
        return old;
    }
    let alpha = (coefficient * dt_years).max(0.0);
    (old + alpha * downstream) / (1.0 + alpha)
}

pub(crate) fn evolve(cells: &mut [Cell], config: &WorldConfig) -> ErosionReport {
    let mut report = ErosionReport {
        steps: config.erosion_steps,
        simulated_years: if config.erosion_steps == 0 {
            0.0
        } else {
            config.geological_years
        },
        ..Default::default()
    };
    let initial: Vec<f64> = cells.iter().map(|c| f64::from(c.height_m)).collect();
    let mut bedrock = initial.clone();
    let mut sediment = vec![0.0; cells.len()];
    let dt = config.geological_years / f64::from(config.erosion_steps.max(1));
    for _ in 0..config.erosion_steps {
        hydrology::route_potential(cells, config.resolution);
        for (i, c) in cells.iter().enumerate() {
            if !c.ocean {
                let uplift = f64::from(c.uplift_m_per_year) * dt;
                bedrock[i] += uplift;
                report.uplift_volume_m3 += uplift * c.area_m2;
            }
        }
        write_heights(cells, &bedrock, &sediment);
        let mut order = hydrology::route_potential(cells, config.resolution);
        let mut generated = vec![0.0; cells.len()];
        // A minor DEM sink is opened by incision of its spill corridor, not painted
        // as a full lake. Every excavated kilogram enters sediment transport.
        let groups = hydrology::depressions(cells, config.resolution, &order);
        for group in groups {
            let depth = group.level - f64::from(cells[group.pit].height_m);
            if depth > config.relief_m * 0.035 && group.cells.len() > 3 {
                continue;
            }
            report.breached_depressions += 1;
            let mut i = group.pit;
            let mut target = height(i, cells, &bedrock, &sediment);
            let mut visits = 0;
            while let Some(next) = cells[i].receiver {
                if cells[next].ocean {
                    break;
                }
                target -= hydrology::distance(&cells[i], &cells[next], config.radius_m) * 0.00002;
                let current = height(next, cells, &bedrock, &sediment);
                if current <= target {
                    break;
                }
                generated[next] += remove(
                    next,
                    current - target,
                    cells,
                    &mut bedrock,
                    &mut sediment,
                    &mut report,
                );
                i = next;
                visits += 1;
                if visits > cells.len() {
                    break;
                }
            }
        }
        write_heights(cells, &bedrock, &sediment);
        order = hydrology::route_potential(cells, config.resolution);
        crate::climate(cells, config);
        let mut water: Vec<f64> = cells.iter().map(hydrology::local_runoff).collect();
        let previous_surface: Vec<f64> = (0..cells.len())
            .map(|i| height(i, cells, &bedrock, &sediment))
            .collect();
        for &i in order.iter().rev() {
            if let Some(j) = cells[i].receiver {
                water[j] += water[i];
            }
        }
        for &i in &order {
            if cells[i].ocean {
                continue;
            }
            let Some(j) = cells[i].receiver else {
                continue;
            };
            let old = height(i, cells, &bedrock, &sediment);
            if f64::from(cells[i].routing_height_m) > old + 1.0 {
                continue;
            }
            let downstream = height(j, cells, &bedrock, &sediment).max(
                if cells[j].routing_height_m - cells[j].height_m > 1.0 {
                    f64::from(cells[j].routing_height_m)
                } else {
                    f64::NEG_INFINITY
                },
            );
            let length = hydrology::distance(&cells[i], &cells[j], config.radius_m).max(1.0);
            let wet_area = water[i] * YEAR_SECONDS / 0.4;
            let coefficient =
                config.stream_power_k_per_year * f64::from(cells[i].erodability) * wet_area.sqrt()
                    / length;
            let new = implicit_height(old, downstream, coefficient, dt);
            let depth = (old - new).max(0.0);
            if depth > 0.01 {
                report.incised_cells += 1;
            }
            generated[i] += remove(i, depth, cells, &mut bedrock, &mut sediment, &mut report);
        }
        let mut flux = generated;
        let mut marine_pool = 0.0;
        for &i in order.iter().rev() {
            if cells[i].ocean {
                let accommodation = (-2.0 - height(i, cells, &bedrock, &sediment)).max(0.0)
                    * cells[i].area_m2
                    * SEDIMENT_DENSITY;
                let deposit = (flux[i] * 0.8)
                    .min(accommodation)
                    .min(25.0 * cells[i].area_m2 * SEDIMENT_DENSITY);
                sediment[i] += deposit;
                report.marine_deposited_kg += deposit;
                marine_pool += flux[i] * 0.8 - deposit;
                report.sediment_exported_kg += flux[i] * 0.2;
                continue;
            }
            let Some(j) = cells[i].receiver else {
                sediment[i] += flux[i];
                continue;
            };
            let slope = ((height(i, cells, &bedrock, &sediment)
                - height(j, cells, &bedrock, &sediment))
                / hydrology::distance(&cells[i], &cells[j], config.radius_m).max(1.0))
            .max(0.0);
            let capacity = water[i]
                * YEAR_SECONDS
                * dt
                * SEDIMENT_DENSITY
                * 0.003
                * (slope / 0.01).sqrt().min(4.0);
            let carried = flux[i].min(capacity);
            // Deposition cannot create an arbitrarily high one-cell pile during a
            // long geological step. Overloaded sediment remains in transport when
            // the resolved accommodation is full, rather than becoming a spike.
            let ceiling = if cells[i].routing_height_m - cells[i].height_m > 1.0 {
                f64::from(cells[i].routing_height_m)
            } else {
                previous_surface[i] + 25.0
            };
            let accommodation = (ceiling - height(i, cells, &bedrock, &sediment)).max(0.0)
                * cells[i].area_m2
                * SEDIMENT_DENSITY;
            let deposited = (flux[i] - carried).min(accommodation);
            sediment[i] += deposited;
            flux[j] += flux[i] - deposited;
        }
        // A coarse marine reservoir disperses sediment beyond river-mouth cells.
        // Accommodation-weighted deposition conserves kilograms and leaves the
        // explicitly exported fraction in the unresolved offshore reservoir.
        if marine_pool > 0.0 {
            let total_capacity: f64 = (0..cells.len())
                .filter(|&i| cells[i].ocean)
                .map(|i| {
                    (-2.0 - height(i, cells, &bedrock, &sediment)).max(0.0)
                        * cells[i].area_m2
                        * SEDIMENT_DENSITY
                })
                .sum();
            let fraction = (marine_pool / total_capacity.max(1.0)).min(1.0);
            let mut deposited = 0.0;
            for i in 0..cells.len() {
                if cells[i].ocean {
                    let mass = (-2.0 - height(i, cells, &bedrock, &sediment)).max(0.0)
                        * cells[i].area_m2
                        * SEDIMENT_DENSITY
                        * fraction;
                    sediment[i] += mass;
                    deposited += mass;
                }
            }
            report.marine_deposited_kg += deposited;
            report.sediment_exported_kg += marine_pool - deposited;
        }
        // Pairwise hillslope transfers conserve mass even with unequal cell area
        // and bedrock density. A local cap prevents explicit-step overshoot.
        if config.hillslope_diffusivity_m2_per_year > 0.0 {
            for i in 0..cells.len() {
                for j in neighbours(i, config.resolution) {
                    if j <= i {
                        continue;
                    }
                    let hi = height(i, cells, &bedrock, &sediment);
                    let hj = height(j, cells, &bedrock, &sediment);
                    let (high, low, difference) = if hi > hj {
                        (i, j, hi - hj)
                    } else {
                        (j, i, hj - hi)
                    };
                    let length =
                        hydrology::distance(&cells[i], &cells[j], config.radius_m).max(1.0);
                    let area = cells[i].area_m2.min(cells[j].area_m2);
                    let volume =
                        (config.hillslope_diffusivity_m2_per_year * dt * difference * area.sqrt()
                            / length)
                            .min(0.08 * difference * area);
                    let transfer = remove(
                        high,
                        volume / cells[high].area_m2,
                        cells,
                        &mut bedrock,
                        &mut sediment,
                        &mut report,
                    );
                    sediment[low] += transfer;
                }
            }
        }
        write_heights(cells, &bedrock, &sediment);
        let stored: f64 = sediment.iter().sum();
        let residual = (report.bedrock_eroded_kg - stored - report.sediment_exported_kg).abs()
            / report.bedrock_eroded_kg.max(1.0);
        report.max_step_mass_balance_relative_error =
            report.max_step_mass_balance_relative_error.max(residual);
    }
    // Last deposition/diffusion can leave a new sub-resolution one-cell dam.
    // Open these actual spill corridors after the split step, and send every
    // excavated kilogram to the explicitly tracked offshore sediment reservoir.
    // Extended depressions remain eligible for the physical lake budget.
    if config.erosion_steps > 0 {
        for _ in 0..4 {
            write_heights(cells, &bedrock, &sediment);
            let order = hydrology::route_potential(cells, config.resolution);
            let groups = hydrology::depressions(cells, config.resolution, &order);
            let mut moved = 0.0;
            for group in groups.into_iter().filter(|g| g.cells.len() <= 3) {
                let mut i = group.pit;
                let mut target = height(i, cells, &bedrock, &sediment);
                let mut visits = 0;
                while let Some(next) = cells[i].receiver {
                    if cells[next].ocean {
                        break;
                    }
                    target -=
                        hydrology::distance(&cells[i], &cells[next], config.radius_m) * 0.00002;
                    let current = height(next, cells, &bedrock, &sediment);
                    if current <= target {
                        break;
                    }
                    moved += remove(
                        next,
                        current - target,
                        cells,
                        &mut bedrock,
                        &mut sediment,
                        &mut report,
                    );
                    i = next;
                    visits += 1;
                    if visits > cells.len() {
                        break;
                    }
                }
                report.breached_depressions += 1;
            }
            report.sediment_exported_kg += moved;
            if moved == 0.0 {
                break;
            }
        }
    }
    write_heights(cells, &bedrock, &sediment);
    report.sediment_stored_kg = sediment.iter().sum();
    report.mass_balance_relative_error =
        (report.bedrock_eroded_kg - report.sediment_stored_kg - report.sediment_exported_kg).abs()
            / report.bedrock_eroded_kg.max(1.0);
    let mut rms = 0.0;
    let mut area = 0.0;
    for (i, c) in cells.iter().enumerate() {
        let change = f64::from(c.height_m) - initial[i];
        rms += change * change * c.area_m2;
        area += c.area_m2;
        report.max_height_change_m = report.max_height_change_m.max(change.abs());
    }
    report.rms_height_change_m = (rms / area).sqrt();
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backward_euler_has_exact_reference_and_no_overshoot() {
        assert_eq!(implicit_height(100.0, 0.0, 0.1, 10.0), 50.0);
        assert!(implicit_height(100.0, 20.0, 0.1, 1e9) >= 20.0);
        assert_eq!(implicit_height(10.0, 20.0, 1.0, 100.0), 10.0);
    }
}
