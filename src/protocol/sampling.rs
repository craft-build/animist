//! Shared sampling-control validation across protocol boundaries.

use snafu::{Snafu, ensure};

use crate::provider::RequestOptions;

/// Sampling control values rejected by a protocol boundary.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum SamplingError {
    #[snafu(display("{boundary}: {control} must be finite and in [{min}, {max}], got {value}"))]
    OutOfRange {
        boundary: &'static str,
        control: &'static str,
        value: f64,
        min: f64,
        max: f64,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("{boundary} does not support explicit {control}"))]
    Unsupported {
        boundary: &'static str,
        control: &'static str,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("{boundary}: {control} conflicts with {other}"))]
    Conflict {
        boundary: &'static str,
        control: &'static str,
        other: &'static str,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("{boundary}: {count} stop sequences exceed the limit {max}"))]
    TooManyStops {
        boundary: &'static str,
        count: usize,
        max: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("{boundary}: stop sequence {index} is empty"))]
    EmptyStop {
        boundary: &'static str,
        index: usize,
        backtrace: snafu::Backtrace,
    },
}

pub(crate) fn validate(
    options: &RequestOptions,
    boundary: &'static str,
    temperature_max: f64,
    max_stops: usize,
) -> super::Result<()> {
    for (control, value, min, max) in [
        ("temperature", options.temperature, 0.0, temperature_max),
        ("top_p", options.top_p, 0.0, 1.0),
        ("frequency_penalty", options.frequency_penalty, -2.0, 2.0),
        ("presence_penalty", options.presence_penalty, -2.0, 2.0),
    ] {
        if let Some(value) = value {
            ensure!(
                value.is_finite() && (min..=max).contains(&value),
                OutOfRangeSnafu {
                    boundary,
                    control,
                    value,
                    min,
                    max,
                }
            );
        }
    }
    if let Some(stops) = &options.stop_sequences {
        ensure!(
            stops.len() <= max_stops,
            TooManyStopsSnafu {
                boundary,
                count: stops.len(),
                max: max_stops,
            }
        );
        for (index, stop) in stops.iter().enumerate() {
            ensure!(!stop.is_empty(), EmptyStopSnafu { boundary, index });
        }
    }
    Ok(())
}
