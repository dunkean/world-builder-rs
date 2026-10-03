//! Implicit n=1 stream-power incision, mass-conserving sediment routing, and
//! conservative hillslope/bank transport. Units are metres, years and kilograms.
//! The implicit downstream-first integration follows Braun & Willett (2013),
//! doi:10.1016/j.geomorph.2012.10.008. Sediment capacity is a reduced closure,
//! not an implementation of the full FastScape transport/deposition equation.
//! Excess sediment relaxes over a finite travel distance (Davy & Lague 2009,
//! doi:10.1029/2008JF001146); its coefficients remain prototype closures.

use crate::{
    geometry::neighbours,
    hydrology::{self, YEAR_SECONDS},
    Cell, Material, WorldConfig,
};
use serde::{Deserialize, Serialize};

const SEDIMENT_DENSITY: f64 = 1700.0;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
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
    /// Material taken from resolved banks and carried by their adjacent channel.
    pub bank_mobilized_kg: f64,
    /// Cumulative continental deposition, including material later remobilized.
    pub fluvial_deposited_kg: f64,
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

fn downstream_level(i: usize, cells: &[Cell], bedrock: &[f64], sediment: &[f64]) -> f64 {
    // Terrestrial incision ends at the water surface, never the seabed. Using the
    // seabed as a base level excavated artificial coastal canyons kilometres deep.
    if cells[i].ocean {
        0.0
    } else if cells[i].routing_height_m - cells[i].height_m > 1.0 {
        height(i, cells, bedrock, sediment).max(f64::from(cells[i].routing_height_m))
    } else {
        height(i, cells, bedrock, sediment)
    }
}

fn deposition_fraction(length: f64, discharge: f64, slope: f64) -> f64 {
    // A cell is not an instantaneous sediment trap. Excess load settles gradually;
    // larger streams and steeper reaches carry it farther. The minimum two-cell
    // relaxation length also bounds deposition during a long geological step.
    let travel = (30_000.0
        * (discharge.max(1.0) / 120.0).powf(0.3)
        * (slope / 0.002).sqrt().clamp(0.25, 4.0))
    .max(2.0 * length);
    -(-length / travel).exp_m1()
}

fn breach_minor_basins(
    cells: &[Cell],
    config: &WorldConfig,
    order: &[usize],
    bedrock: &mut [f64],
    sediment: &mut [f64],
    report: &mut ErosionReport,
) -> Vec<f64> {
    let mut generated = vec![0.0; cells.len()];
    for group in hydrology::depressions(cells, config.resolution, order) {
        let depth = group.level - height(group.pit, cells, bedrock, sediment);
        // Depth, rather than a one-cell shape heuristic, distinguishes minor
        // sediment dams from potentially genuine reservoirs.
        if depth >= config.lake_min_basin_depth_m {
            continue;
        }
        let mut i = group.pit;
        let mut target = height(i, cells, bedrock, sediment);
        let mut mobilized = false;
        for _ in 0..cells.len() {
            let Some(next) = cells[i].receiver else { break };
            if cells[next].ocean {
                break;
            }
            target -=
                (hydrology::distance(&cells[i], &cells[next], config.radius_m) * 1e-7).max(0.002);
            let current = height(next, cells, bedrock, sediment);
            if current <= target {
                break;
            }
            generated[next] += remove(next, current - target, cells, bedrock, sediment, report);
            mobilized = true;
            i = next;
        }
        if mobilized {
            report.breached_depressions += 1;
        }
    }
    generated
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
        // A minor DEM sink is opened by incision of its spill corridor, not painted
        // as a full lake. Every excavated kilogram enters sediment transport.
        let mut generated = breach_minor_basins(
            cells,
            config,
            &order,
            &mut bedrock,
            &mut sediment,
            &mut report,
        );
        write_heights(cells, &bedrock, &sediment);
        order = hydrology::route_potential(cells, config.resolution);
        crate::climate(cells, config);
        let mut water: Vec<f64> = cells.iter().map(hydrology::local_runoff).collect();
        let mut contributing: Vec<f64> = cells.iter().map(|c| c.area_m2).collect();
        let previous_surface: Vec<f64> = (0..cells.len())
            .map(|i| height(i, cells, &bedrock, &sediment))
            .collect();
        for &i in order.iter().rev() {
            if let Some(j) = cells[i].receiver {
                water[j] += water[i];
                contributing[j] += contributing[i];
            }
        }
        let mut incision = vec![0.0; cells.len()];
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
            let downstream = downstream_level(j, cells, &bedrock, &sediment);
            let length = hydrology::distance(&cells[i], &cells[j], config.radius_m).max(1.0);
            let wet_area = water[i] * YEAR_SECONDS / 0.4;
            // Suppress sheet-like incision on divides, while retaining the full
            // stream-power coefficient once several upstream cells converge.
            let upstream = ((contributing[i] - cells[i].area_m2) / cells[i].area_m2).max(0.0);
            let activation = 0.12 + 0.88 * -(-upstream / 3.0).exp_m1();
            let coefficient = activation
                * config.stream_power_k_per_year
                * f64::from(cells[i].erodability)
                * wet_area.sqrt()
                / length;
            let new = implicit_height(old, downstream, coefficient, dt);
            let depth = (old - new).max(0.0);
            incision[i] = depth;
            if depth > 0.01 {
                report.incised_cells += 1;
            }
            generated[i] += remove(i, depth, cells, &mut bedrock, &mut sediment, &mut report);
        }
        // Incision destabilizes the immediately resolved banks. Material leaves
        // the bank and enters the adjacent stream, rather than reappearing there
        // as a compensating pile. This reduced undercutting closure is conservative
        // and strengthens valley shoulders without changing any sampling/LOD rule.
        for &i in &order {
            if cells[i].ocean || incision[i] <= 0.0 || contributing[i] < 4.0 * cells[i].area_m2 {
                continue;
            }
            let channel = height(i, cells, &bedrock, &sediment);
            for j in hydrology::neighbours8(i, config.resolution) {
                if cells[j].ocean
                    || water[j] > 0.35 * water[i]
                    || cells[j].routing_height_m - cells[j].height_m > 1.0
                {
                    continue;
                }
                let relief = (height(j, cells, &bedrock, &sediment) - channel).max(0.0);
                let length = hydrology::distance(&cells[i], &cells[j], config.radius_m).max(1.0);
                let depth = (0.25 * incision[i] * relief / length).min(0.1 * relief);
                let mass = remove(j, depth, cells, &mut bedrock, &mut sediment, &mut report);
                generated[i] += mass;
                report.bank_mobilized_kg += mass;
            }
        }
        let mut flux = generated;
        let mut marine_pool = 0.0;
        let mut incoming_ceiling = vec![f64::INFINITY; cells.len()];
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
            let length = hydrology::distance(&cells[i], &cells[j], config.radius_m).max(1.0);
            let slope = ((height(i, cells, &bedrock, &sediment)
                - downstream_level(j, cells, &bedrock, &sediment))
                / length)
                .max(0.0);
            let capacity = water[i]
                * YEAR_SECONDS
                * dt
                * SEDIMENT_DENSITY
                * 0.003
                * (slope / 0.01).sqrt().min(4.0);
            // Deposition cannot create an arbitrarily high one-cell pile during a
            // long geological step. Overloaded sediment remains in transport when
            // the resolved accommodation is full, rather than becoming a spike.
            let ceiling = if cells[i].routing_height_m - cells[i].height_m > 1.0 {
                f64::from(cells[i].routing_height_m)
            } else {
                (previous_surface[i] + 25.0).min(incoming_ceiling[i])
            };
            let accommodation = (ceiling - height(i, cells, &bedrock, &sediment)).max(0.0)
                * cells[i].area_m2
                * SEDIMENT_DENSITY;
            let deposited = ((flux[i] - capacity).max(0.0)
                * deposition_fraction(length, water[i], slope))
            .min(accommodation);
            sediment[i] += deposited;
            report.fluvial_deposited_kg += deposited;
            flux[j] += flux[i] - deposited;
            if cells[i].routing_height_m - cells[i].height_m <= 1.0 {
                incoming_ceiling[j] = incoming_ceiling[j]
                    .min(height(i, cells, &bedrock, &sediment) - length * 0.000002);
            }
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
    // Deep depressions remain eligible for the physical lake budget.
    if config.erosion_steps > 0 {
        for _ in 0..4 {
            write_heights(cells, &bedrock, &sediment);
            let order = hydrology::route_potential(cells, config.resolution);
            let moved: f64 = breach_minor_basins(
                cells,
                config,
                &order,
                &mut bedrock,
                &mut sediment,
                &mut report,
            )
            .iter()
            .sum();
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

/// Reopen minor sediment dams after the final sea-datum adjustment. The caller
/// must reroute water afterwards. Genuine deep basins remain untouched; excavated
/// material is accounted in the unresolved offshore export, never discarded.
pub(crate) fn finish_minor_basins(
    cells: &mut [Cell],
    config: &WorldConfig,
    report: &mut ErosionReport,
) {
    if config.erosion_steps == 0 || config.lake_min_basin_depth_m <= 0.0 {
        return;
    }
    let mut bedrock: Vec<f64> = cells.iter().map(|c| f64::from(c.bedrock_m)).collect();
    let mut sediment: Vec<f64> = cells
        .iter()
        .map(|c| f64::from(c.sediment_m) * c.area_m2 * SEDIMENT_DENSITY)
        .collect();
    let starting_sediment: f64 = sediment.iter().sum();
    for _ in 0..4 {
        let order = hydrology::route_potential(cells, config.resolution);
        let generated =
            breach_minor_basins(cells, config, &order, &mut bedrock, &mut sediment, report);
        let moved: f64 = generated.iter().sum();
        // Preserve bit-identical unmodified terrain, including f32 rounding from
        // the datum adjustment. Only physically excavated cells are rewritten.
        for (i, mass) in generated.iter().enumerate() {
            if *mass > 0.0 {
                cells[i].bedrock_m = bedrock[i] as f32;
                cells[i].sediment_m = (sediment[i] / (cells[i].area_m2 * SEDIMENT_DENSITY)) as f32;
                cells[i].height_m = (bedrock[i] + f64::from(cells[i].sediment_m)) as f32;
            }
        }
        report.sediment_exported_kg += moved;
        if moved == 0.0 {
            break;
        }
    }
    // Use a difference so the pre-existing f64 mass ledger is not replaced by
    // a second sum reconstructed from the persisted f32 sediment thickness.
    report.sediment_stored_kg += sediment.iter().sum::<f64>() - starting_sediment;
    report.mass_balance_relative_error =
        (report.bedrock_eroded_kg - report.sediment_stored_kg - report.sediment_exported_kg).abs()
            / report.bedrock_eroded_kg.max(1.0);
    report.max_step_mass_balance_relative_error = report
        .max_step_mass_balance_relative_error
        .max(report.mass_balance_relative_error);
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

    fn synthetic_cells() -> (Vec<Cell>, WorldConfig) {
        let config = WorldConfig {
            resolution: 8,
            erosion_steps: 0,
            ..Default::default()
        };
        let mut cells = crate::World::generate(config.clone()).unwrap().cells;
        for c in &mut cells {
            c.height_m = 20.0;
            c.bedrock_m = 20.0;
            c.sediment_m = 0.0;
            c.uplift_m_per_year = 0.0;
            c.ocean = false;
            c.lake = false;
            c.lake_id = None;
            c.water_m = 20.0;
            c.moisture = 0.6;
            c.temperature_c = 15.0;
        }
        cells[0].height_m = -1000.0;
        cells[0].bedrock_m = -1000.0;
        (cells, config)
    }

    #[test]
    fn marine_base_level_is_independent_of_seabed_depth() {
        let (mut cells, _) = synthetic_cells();
        let sediment = vec![0.0; cells.len()];
        cells[0].ocean = true;
        cells[0].routing_height_m = 0.0;
        let mut bedrock: Vec<f64> = cells.iter().map(|c| f64::from(c.height_m)).collect();
        assert_eq!(downstream_level(0, &cells, &bedrock, &sediment), 0.0);
        bedrock[0] = -8000.0;
        assert_eq!(downstream_level(0, &cells, &bedrock, &sediment), 0.0);
        assert!(
            implicit_height(
                10.0,
                downstream_level(0, &cells, &bedrock, &sediment),
                1.0,
                1e8
            ) >= 0.0
        );
    }

    #[test]
    fn excess_sediment_is_not_instantly_deposited_in_one_cell() {
        let weak = deposition_fraction(1000.0, 120.0, 0.001);
        let strong = deposition_fraction(1000.0, 12_000.0, 0.001);
        assert!(weak > 0.0 && weak < 0.1);
        assert!(strong > 0.0 && strong < weak);
        assert!(deposition_fraction(100_000.0, 1.0, 0.0) < 0.4);
    }

    #[test]
    fn shallow_dam_is_excavated_with_closed_sediment_ledger() {
        let (mut cells, mut config) = synthetic_cells();
        cells[200].height_m = 15.0;
        cells[200].bedrock_m = 15.0;
        config.erosion_steps = 1;
        let before: Vec<f32> = cells.iter().map(|c| c.height_m).collect();
        let mut report = ErosionReport::default();
        finish_minor_basins(&mut cells, &config, &mut report);
        let excavated: f64 = cells
            .iter()
            .zip(&before)
            .map(|(c, h)| {
                (f64::from(*h) - f64::from(c.height_m)).max(0.0)
                    * c.area_m2
                    * rock_density(c.material)
            })
            .sum();
        assert!(excavated > 0.0);
        assert!(report.breached_depressions > 0);
        assert!(report.mass_balance_relative_error < 1e-12);
        assert!((excavated - report.bedrock_eroded_kg).abs() / excavated < 1e-6);
        assert!(cells.iter().zip(&before).all(|(c, h)| c.height_m <= *h));
        let order = hydrology::route_potential(&mut cells, config.resolution);
        assert!(hydrology::depressions(&cells, config.resolution, &order)
            .iter()
            .all(|g| {
                g.level - f64::from(cells[g.pit].height_m) >= config.lake_min_basin_depth_m
            }));
    }

    #[test]
    fn deep_single_cell_lake_is_not_breached_by_shape_heuristic() {
        let (mut cells, mut config) = synthetic_cells();
        cells[200].height_m = 5.0;
        cells[200].bedrock_m = 5.0;
        config.erosion_steps = 1;
        let before: Vec<f32> = cells.iter().map(|c| c.height_m).collect();
        let mut report = ErosionReport::default();
        finish_minor_basins(&mut cells, &config, &mut report);
        assert_eq!(report.breached_depressions, 0);
        assert_eq!(report.bedrock_eroded_kg, 0.0);
        assert_eq!(before, cells.iter().map(|c| c.height_m).collect::<Vec<_>>());
    }

    #[test]
    fn finishing_alluvial_dam_preserves_previous_mass_ledger_and_is_idempotent() {
        let (mut cells, mut config) = synthetic_cells();
        config.erosion_steps = 1;
        for c in cells.iter_mut().skip(1) {
            c.bedrock_m = 10.0;
            c.sediment_m = 10.0;
        }
        cells[200].height_m = 15.0;
        cells[200].sediment_m = 5.0;
        let stored_before: f64 = cells
            .iter()
            .map(|c| f64::from(c.sediment_m) * c.area_m2 * SEDIMENT_DENSITY)
            .sum();
        // The initial sediment represents material already mobilized during
        // evolution: finishing must update this ledger, not restart it from f32.
        let mut report = ErosionReport {
            bedrock_eroded_kg: stored_before,
            sediment_stored_kg: stored_before,
            ..Default::default()
        };
        finish_minor_basins(&mut cells, &config, &mut report);
        let stored_after: f64 = cells
            .iter()
            .map(|c| f64::from(c.sediment_m) * c.area_m2 * SEDIMENT_DENSITY)
            .sum();
        assert!(stored_after < stored_before);
        assert!(report.sediment_exported_kg > 0.0);
        assert!(report.mass_balance_relative_error < 1e-12);
        assert!(
            (stored_after + report.sediment_exported_kg - report.bedrock_eroded_kg).abs()
                / report.bedrock_eroded_kg
                < 1e-6
        );
        let heights: Vec<_> = cells
            .iter()
            .map(|c| {
                (
                    c.height_m.to_bits(),
                    c.bedrock_m.to_bits(),
                    c.sediment_m.to_bits(),
                )
            })
            .collect();
        let exported = report.sediment_exported_kg;
        finish_minor_basins(&mut cells, &config, &mut report);
        assert_eq!(report.sediment_exported_kg, exported);
        assert_eq!(
            heights,
            cells
                .iter()
                .map(|c| (
                    c.height_m.to_bits(),
                    c.bedrock_m.to_bits(),
                    c.sediment_m.to_bits()
                ))
                .collect::<Vec<_>>()
        );
    }
}
