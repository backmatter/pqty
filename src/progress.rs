//! Versioned, line-delimited progress events for process Consumers.
//!
//! Progress is an advisory CLI stream, kept on stderr so Artifact Protocol
//! documents on stdout remain unchanged. The default human mode is useful for
//! direct pqty invocations; Consumers request JSON explicitly and negotiate
//! `pqty.progress/v1` through `pqty.capabilities/v1`.

use std::cell::Cell;
use std::io::{IsTerminal as _, Write as _};

use serde::Serialize;

pub const PROGRESS_SCHEMA: &str = "pqty.progress/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum ProgressOutput {
    Human,
    Json,
    Off,
}

thread_local! {
    static OUTPUT: Cell<ProgressOutput> = const { Cell::new(ProgressOutput::Human) };
}

pub(crate) fn configure(output: ProgressOutput) {
    OUTPUT.set(output);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DownloadCategory {
    Registry,
    Packages,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub(crate) enum ProgressEvent<'a> {
    #[serde(rename = "download-plan")]
    Plan {
        schema: &'static str,
        category: DownloadCategory,
        items_total: usize,
        items_cached: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        bytes_total: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        bytes_cached: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        bytes_to_download: Option<u64>,
    },
    #[serde(rename = "download-start")]
    Start {
        schema: &'static str,
        category: DownloadCategory,
        item: &'a str,
        url: &'a str,
        attempt: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        bytes_total: Option<u64>,
    },
    #[serde(rename = "download-progress")]
    Progress {
        schema: &'static str,
        category: DownloadCategory,
        item: &'a str,
        bytes_received: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        bytes_total: Option<u64>,
        elapsed_millis: u64,
    },
    #[serde(rename = "download-complete")]
    Complete {
        schema: &'static str,
        category: DownloadCategory,
        item: &'a str,
        bytes_received: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        bytes_total: Option<u64>,
        elapsed_millis: u64,
    },
}

pub(crate) fn download_plan(
    category: DownloadCategory,
    items_total: usize,
    items_cached: usize,
    bytes_total: Option<u64>,
    bytes_cached: Option<u64>,
) {
    emit(&ProgressEvent::Plan {
        schema: PROGRESS_SCHEMA,
        category,
        items_total,
        items_cached,
        bytes_total,
        bytes_cached,
        bytes_to_download: match (bytes_total, bytes_cached) {
            (Some(total), Some(cached)) => Some(total.saturating_sub(cached)),
            _ => None,
        },
    });
}

pub(crate) fn download_start(
    category: DownloadCategory,
    item: &str,
    url: &str,
    attempt: usize,
    bytes_total: Option<u64>,
) {
    emit(&ProgressEvent::Start {
        schema: PROGRESS_SCHEMA,
        category,
        item,
        url,
        attempt,
        bytes_total,
    });
}

pub(crate) fn download_progress(
    category: DownloadCategory,
    item: &str,
    bytes_received: u64,
    bytes_total: Option<u64>,
    elapsed_millis: u64,
) {
    emit(&ProgressEvent::Progress {
        schema: PROGRESS_SCHEMA,
        category,
        item,
        bytes_received,
        bytes_total,
        elapsed_millis,
    });
}

pub(crate) fn download_complete(
    category: DownloadCategory,
    item: &str,
    bytes_received: u64,
    bytes_total: Option<u64>,
    elapsed_millis: u64,
) {
    emit(&ProgressEvent::Complete {
        schema: PROGRESS_SCHEMA,
        category,
        item,
        bytes_received,
        bytes_total,
        elapsed_millis,
    });
}

fn emit(event: &ProgressEvent<'_>) {
    OUTPUT.with(|output| match output.get() {
        ProgressOutput::Human => emit_human(event),
        ProgressOutput::Json => {
            let stderr = std::io::stderr();
            let mut stderr = stderr.lock();
            if serde_json::to_writer(&mut stderr, &event).is_ok() {
                let _ = stderr.write_all(b"\n");
                let _ = stderr.flush();
            }
        }
        ProgressOutput::Off => {}
    });
}

fn emit_human(event: &ProgressEvent<'_>) {
    match event {
        ProgressEvent::Plan {
            category,
            items_total,
            items_cached,
            bytes_total,
            bytes_cached,
            bytes_to_download,
            ..
        } => {
            let label = category_label(*category);
            eprintln!(
                "pqty: {}",
                human_plan_message(
                    label,
                    *items_total,
                    *items_cached,
                    *bytes_total,
                    *bytes_cached,
                    *bytes_to_download,
                )
            );
        }
        ProgressEvent::Start {
            item,
            url,
            attempt,
            bytes_total,
            ..
        } => {
            let retry = if *attempt == 1 { "" } else { "retrying " };
            let size =
                bytes_total.map_or_else(String::new, |bytes| format!(" ({})", human_bytes(bytes)));
            eprintln!("pqty: {retry}fetching {item}{size} from {url}");
        }
        ProgressEvent::Progress {
            item,
            bytes_received,
            bytes_total,
            elapsed_millis,
            ..
        } if std::io::stderr().is_terminal() => {
            let rate = bytes_per_second(*bytes_received, *elapsed_millis);
            let progress = bytes_total.map_or_else(
                || human_bytes(*bytes_received),
                |total| {
                    let percent = bytes_received.saturating_mul(100) / total.max(1);
                    format!(
                        "{}/{} ({percent}%)",
                        human_bytes(*bytes_received),
                        human_bytes(total)
                    )
                },
            );
            eprint!(
                "\rpqty: downloading {item}: {progress} · {}/s",
                human_bytes(rate)
            );
            let _ = std::io::stderr().flush();
        }
        ProgressEvent::Complete {
            item,
            bytes_received,
            elapsed_millis,
            ..
        } => {
            let rate = bytes_per_second(*bytes_received, *elapsed_millis);
            let prefix = if std::io::stderr().is_terminal() {
                "\r"
            } else {
                ""
            };
            eprintln!(
                "{prefix}pqty: downloaded {item}: {} in {} · {}/s",
                human_bytes(*bytes_received),
                human_duration(*elapsed_millis),
                human_bytes(rate)
            );
        }
        ProgressEvent::Progress { .. } => {}
    }
}

const fn category_label(category: DownloadCategory) -> &'static str {
    match category {
        DownloadCategory::Registry => "Registry Snapshot",
        DownloadCategory::Packages => "Package containers",
    }
}

fn human_plan_message(
    label: &str,
    items_total: usize,
    items_cached: usize,
    bytes_total: Option<u64>,
    bytes_cached: Option<u64>,
    bytes_to_download: Option<u64>,
) -> String {
    match (bytes_total, bytes_cached, bytes_to_download) {
        (Some(total), Some(_), Some(0)) => format!(
            "{label}: all {}, {}, {} cached",
            count_noun(items_total, "item", "items"),
            human_bytes(total),
            if items_total == 1 { "is" } else { "are" }
        ),
        (Some(total), Some(cached), Some(download)) => format!(
            "{label}: {} to download across {}; {} of {} cached",
            human_bytes(download),
            count_noun(items_total.saturating_sub(items_cached), "item", "items"),
            human_bytes(cached),
            human_bytes(total)
        ),
        _ => format!("{label}: download size is not declared by the server"),
    }
}

fn count_noun(count: usize, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}

fn bytes_per_second(bytes: u64, elapsed_millis: u64) -> u64 {
    bytes.saturating_mul(1_000) / elapsed_millis.max(1)
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut divisor = 1_u64;
    let mut unit = 0_usize;
    while bytes / divisor >= 1024 && unit < UNITS.len() - 1 {
        divisor *= 1024;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        let rounded_tenths =
            (u128::from(bytes) * 10 + u128::from(divisor / 2)) / u128::from(divisor);
        format!(
            "{}.{} {}",
            rounded_tenths / 10,
            rounded_tenths % 10,
            UNITS[unit]
        )
    }
}

fn human_duration(millis: u64) -> String {
    let seconds = millis / 1_000;
    if seconds == 0 {
        "<1s".to_string()
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use crate::progress::{
        DownloadCategory, PROGRESS_SCHEMA, ProgressEvent, category_label, human_plan_message,
    };

    #[test]
    fn progress_events_are_closed_versioned_json_lines() {
        let event = ProgressEvent::Plan {
            schema: PROGRESS_SCHEMA,
            category: DownloadCategory::Packages,
            items_total: 3,
            items_cached: 2,
            bytes_total: Some(9_000),
            bytes_cached: Some(4_000),
            bytes_to_download: Some(5_000),
        };
        assert_eq!(
            serde_json::to_value(event).expect("progress JSON"),
            serde_json::json!({
                "schema": "pqty.progress/v1",
                "event": "download-plan",
                "category": "packages",
                "items_total": 3,
                "items_cached": 2,
                "bytes_total": 9000,
                "bytes_cached": 4000,
                "bytes_to_download": 5000
            })
        );
    }

    #[test]
    fn human_download_plans_use_consistent_labels_and_grammar() {
        let transcript = [
            human_plan_message(
                category_label(DownloadCategory::Registry),
                1,
                1,
                Some(1_024),
                Some(1_024),
                Some(0),
            ),
            human_plan_message(
                category_label(DownloadCategory::Packages),
                3,
                1,
                Some(9_000),
                Some(4_000),
                Some(5_000),
            ),
        ]
        .join("\n");
        assert_eq!(
            transcript,
            include_str!("../tests/golden/human/progress-plans.txt").trim_end()
        );
    }
}
