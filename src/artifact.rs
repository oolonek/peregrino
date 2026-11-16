//! Picoto audit and MGF interpretation shared by registration and KG export.
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

pub(crate) type AcceptedFeatures = BTreeMap<String, (String, String)>;

pub(crate) fn audit(path: &Path) -> Result<AcceptedFeatures> {
    let mut table = csv::ReaderBuilder::new()
        .delimiter(b'\t')
        .from_path(path)
        .with_context(|| format!("read audit {}", path.display()))?;
    const HEADERS: [&str; 14] = [
        "feature_id",
        "mz",
        "rt_seconds",
        "apex_intensity",
        "apex_to_baseline",
        "ms1_scans",
        "area",
        "isotope_peaks",
        "quality_score",
        "status",
        "reason",
        "ms2_scan_ids",
        "ms2_quality",
        "collision_energy",
    ];
    ensure!(
        table.headers()?.iter().eq(HEADERS),
        "features must be a picoto audit TSV: {}",
        path.display()
    );
    let mut accepted = AcceptedFeatures::new();
    for record in table.records() {
        let record = record?;
        let id = record.get(0).context("missing feature ID")?;
        match record.get(9).context("missing feature status")? {
            "accepted" => {
                let mz = record.get(1).context("missing feature mz")?;
                let rt = record.get(2).context("missing feature retention time")?;
                ensure!(
                    !id.is_empty()
                        && id.bytes().all(|b| b.is_ascii_digit())
                        && mz.parse::<f64>().is_ok_and(|n| n.is_finite() && n > 0.0)
                        && rt.parse::<f64>().is_ok_and(|n| n.is_finite() && n >= 0.0),
                    "invalid accepted audit feature {id:?}"
                );
                ensure!(
                    accepted
                        .insert(id.to_owned(), (mz.to_owned(), rt.to_owned()))
                        .is_none(),
                    "duplicate accepted audit feature ID {id:?}"
                );
            }
            "rejected" => ensure!(id.is_empty(), "rejected picoto features have no feature ID"),
            status => bail!("invalid audit status {status:?}"),
        }
    }
    Ok(accepted)
}

/// Validate every accepted feature against an MS2-only or paired Picoto MGF.
/// Coordinates permit only Picoto's decimal rounding of precursor m/z and RT.
pub(crate) fn validate_mgf(
    path: &Path,
    raw: &str,
    accepted: &AcceptedFeatures,
    paired: bool,
) -> Result<()> {
    let mut seen_ms1 = HashSet::new();
    let mut seen_ms2 = HashSet::new();
    let mut block = false;
    let mut id = None::<String>;
    let mut level = None::<u8>;
    let mut filename = None::<String>;
    let mut title = None::<String>;
    let mut pepmass = None::<f64>;
    let mut charge = false;
    let mut rt = None::<f64>;
    let mut peaks = false;
    let mut line = String::new();
    let mut reader = BufReader::new(File::open(path)?);
    while reader.read_line(&mut line)? > 0 {
        let entry = line.trim();
        if entry == "BEGIN IONS" {
            ensure!(!block, "nested MGF blocks in {}", path.display());
            block = true;
            id = None;
            level = None;
            filename = None;
            title = None;
            pepmass = None;
            charge = false;
            rt = None;
            peaks = false;
        } else if entry == "END IONS" {
            ensure!(block, "MGF END IONS without BEGIN IONS");
            let feature = id.as_deref().context("MGF block missing FEATURE_ID")?;
            let ms_level = level.context("MGF block missing MSLEVEL")?;
            let title_matches = title
                .as_deref()
                .and_then(|value| value.strip_prefix(raw))
                .and_then(|value| value.strip_prefix(":feature_"))
                .and_then(|value| value.strip_prefix(feature))
                .is_some_and(|value| {
                    value
                        == match ms_level {
                            1 => ":MS1",
                            2 => ":MS2",
                            _ => "",
                        }
                });
            ensure!(
                filename.as_deref() == Some(raw) && title_matches,
                "MGF FILENAME/TITLE do not match registered raw file"
            );
            ensure!(charge && peaks, "MGF block missing picoto charge/peaks");
            let (accepted_id, (audit_mz, audit_rt)) = accepted
                .get_key_value(feature)
                .context("MGF feature absent from accepted audit")?;
            let mz = pepmass.context("MGF block missing PEPMASS")?;
            let retention = rt.context("MGF block missing RTINSECONDS")?;
            ensure!(
                (mz - audit_mz.parse::<f64>()?).abs() <= 0.000_000_51
                    && (retention - audit_rt.parse::<f64>()?).abs() <= 0.000_55,
                "MGF precursor or retention time disagrees with accepted audit feature {feature}"
            );
            let seen = match ms_level {
                1 if paired => &mut seen_ms1,
                2 => &mut seen_ms2,
                _ => bail!("unsupported MSLEVEL {ms_level} in {}", path.display()),
            };
            ensure!(
                seen.insert(accepted_id.as_str()),
                "duplicate FEATURE_ID {feature} at MSLEVEL {ms_level}"
            );
            block = false;
        } else if !entry.is_empty() {
            ensure!(block, "content outside MGF block in {}", path.display());
            if let Some((key, value)) = entry.split_once('=') {
                match key {
                    "FEATURE_ID" => {
                        ensure!(
                            id.replace(value.to_owned()).is_none()
                                && !value.is_empty()
                                && value.bytes().all(|b| b.is_ascii_digit()),
                            "invalid or repeated FEATURE_ID"
                        );
                    }
                    "MSLEVEL" => {
                        ensure!(
                            level
                                .replace(value.parse().context("invalid MSLEVEL")?)
                                .is_none(),
                            "repeated MSLEVEL"
                        );
                    }
                    "FILENAME" => {
                        ensure!(
                            filename.replace(value.to_owned()).is_none(),
                            "repeated FILENAME"
                        );
                    }
                    "TITLE" => {
                        ensure!(title.replace(value.to_owned()).is_none(), "repeated TITLE");
                    }
                    "PEPMASS" => {
                        let mz = value.parse::<f64>().context("invalid PEPMASS")?;
                        ensure!(
                            mz.is_finite() && mz > 0.0 && pepmass.replace(mz).is_none(),
                            "invalid or repeated PEPMASS"
                        );
                    }
                    "CHARGE" => {
                        ensure!(
                            value.ends_with('+') || value.ends_with('-'),
                            "invalid CHARGE"
                        );
                        charge = true;
                    }
                    "RTINSECONDS" => {
                        let seconds = value.parse::<f64>().context("invalid RTINSECONDS")?;
                        ensure!(
                            seconds.is_finite() && seconds >= 0.0 && rt.replace(seconds).is_none(),
                            "invalid or repeated RTINSECONDS"
                        );
                    }
                    _ => {}
                }
            } else {
                let mut values = entry.split_whitespace();
                let mz = values.next().and_then(|x| x.parse::<f64>().ok());
                let intensity = values.next().and_then(|x| x.parse::<f64>().ok());
                ensure!(
                    values.next().is_none()
                        && mz.is_some_and(|x| x.is_finite() && x > 0.0)
                        && intensity.is_some_and(|x| x.is_finite() && x >= 0.0),
                    "invalid MGF peak {entry:?}"
                );
                peaks = true;
            }
        }
        line.clear();
    }
    ensure!(!block, "unclosed MGF block in {}", path.display());
    ensure!(
        seen_ms2.len() == accepted.len() && (!paired || seen_ms1.len() == accepted.len()),
        "MGF FEATURE_IDs do not match accepted audit features"
    );
    Ok(())
}
