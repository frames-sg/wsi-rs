//! Tolerance metrics and diff dumps for parity comparisons.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Tolerance {
    pub max_abs: u8,
    pub mean_abs: f64,
}

impl Tolerance {
    pub(crate) const EXACT: Self = Self {
        max_abs: 0,
        mean_abs: 0.0,
    };

    pub(crate) const JPEG_TIGHT: Self = Self {
        max_abs: 1,
        mean_abs: 0.05,
    };

    pub(crate) const JPEG_DECODER_COMPAT: Self = Self {
        max_abs: 3,
        mean_abs: 0.1,
    };

    pub(crate) const TOLERANT: Self = Self {
        max_abs: 4,
        mean_abs: 1.0,
    };
}

#[derive(Debug, Clone)]
pub(crate) struct CompareReport {
    pub bytewise_equal_rate: f64,
    pub max_abs: u8,
    pub mean_abs: f64,
    pub psnr_db: f64,
    pub alpha_exact: bool,
    pub passed: bool,
    pub diff_dump: Option<PathBuf>,
}

pub(crate) fn compare_rgba(actual: &[u8], expected: &[u8], tol: Tolerance) -> CompareReport {
    assert_eq!(
        actual.len(),
        expected.len(),
        "compare_rgba: length mismatch ({} vs {})",
        actual.len(),
        expected.len()
    );
    assert!(
        actual.len().is_multiple_of(4),
        "compare_rgba: not RGBA-aligned"
    );

    if actual.is_empty() {
        return CompareReport {
            bytewise_equal_rate: 1.0,
            max_abs: 0,
            mean_abs: 0.0,
            psnr_db: f64::INFINITY,
            alpha_exact: true,
            passed: true,
            diff_dump: None,
        };
    }

    let mut equal = 0u64;
    let mut max_abs = 0u8;
    let mut sum_abs = 0u64;
    let mut sum_sq = 0u64;
    let mut alpha_exact = true;
    for (actual, expected) in actual.chunks_exact(4).zip(expected.chunks_exact(4)) {
        alpha_exact &= actual[3] == expected[3];
        for channel in 0..3 {
            if actual[channel] == expected[channel] {
                equal += 1;
            }
            let difference = actual[channel].abs_diff(expected[channel]);
            max_abs = max_abs.max(difference);
            sum_abs += u64::from(difference);
            sum_sq += u64::from(difference) * u64::from(difference);
        }
    }

    // JP2K tolerances apply independently to color channels. Alpha is geometry
    // and coverage information, so it is always exact and never allowed to
    // dilute the RGB mean with a fourth channel of zeros.
    let n = (actual.len() / 4 * 3) as f64;
    let mean_abs = sum_abs as f64 / n;
    let mse = sum_sq as f64 / n;
    let psnr_db = if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0_f64 * 255.0 / mse).log10()
    };
    let passed = alpha_exact && max_abs <= tol.max_abs && mean_abs <= tol.mean_abs;

    CompareReport {
        bytewise_equal_rate: equal as f64 / n,
        max_abs,
        mean_abs,
        psnr_db,
        alpha_exact,
        passed,
        diff_dump: None,
    }
}

pub(crate) fn tolerance_failure(label: &str, report: &CompareReport) -> Option<String> {
    if report.passed {
        return None;
    }
    Some(format!(
        "{label}: exceeds tolerance (max_abs={} mean_abs={:.4} alpha_exact={} psnr={:.2}dB equal_rate={:.4})",
        report.max_abs,
        report.mean_abs,
        report.alpha_exact,
        report.psnr_db,
        report.bytewise_equal_rate
    ))
}
