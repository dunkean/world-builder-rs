//! Run explicitly on CUDA, compare against the CPU reference and print budgets.
use anyhow::{ensure, Result};
use world_erosion::{erode, Backend, ErosionConfig, HeightField};

fn main() -> Result<()> {
    let n = std::env::args()
        .nth(1)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(257);
    ensure!((3..=2049).contains(&n), "grid must be between 3 and 2049");
    let field = HeightField {
        width: n,
        height: n,
        spacing_m: 8.0,
        protected: vec![0; n * n],
        heights_m: (0..n * n)
            .map(|i| {
                let x = (i % n) as f32;
                let y = (i / n) as f32;
                800.0 - y * 0.8 + 22.0 * (x * 0.032).sin() + 8.0 * (x * 0.21 + y * 0.13).sin()
            })
            .collect(),
    };
    let config = ErosionConfig::default();
    // First pass includes NVRTC compilation and driver/context initialization.
    let warmup = erode(&field, ErosionConfig { steps: 1, ..config }, Backend::Cuda)?;
    let gpu = erode(&field, config, Backend::Cuda)?;
    let cpu = erode(&field, config, Backend::Cpu)?;
    let max_difference = gpu
        .residual_m
        .iter()
        .zip(&cpu.residual_m)
        .map(|(&a, &b)| (a - b).abs())
        .fold(0.0, f32::max);
    ensure!(
        max_difference < 0.025,
        "CPU/CUDA differ by {max_difference} m"
    );
    ensure!(gpu.report.eroded_m3 > 0.0, "no erosion occurred");
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "grid":n,"cold_start_ms":warmup.report.elapsed_ms,
            "max_cpu_cuda_difference_m":max_difference,"cuda":gpu.report,"cpu":cpu.report
        }))?
    );
    Ok(())
}
