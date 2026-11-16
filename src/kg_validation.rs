//! Validate an exported graph against the local SHACL shapes without Python or a SPARQL endpoint.
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use indicatif::{ProgressBar, ProgressStyle};
use rudof_rdf::rdf_core::RDFFormat;
use rudof_rdf::rdf_impl::ReaderMode;
use shacl::ir::IRSchema;
use shacl::rdf::ShaclParser;
use shacl::validator::processor::{DataValidation, ShaclProcessor};
use shacl::validator::{ShaclConfig, ShaclValidationMode};
use sparql_service::RdfData;

const SHAPES: &str = include_str!("../kg/shapes.ttl");

pub fn validate(graph: &Path) -> Result<()> {
    let mut data = RdfData::new();
    let input = File::open(graph).with_context(|| format!("opening {}", graph.display()))?;
    let progress = ProgressBar::new(input.metadata()?.len());
    progress.set_style(ProgressStyle::with_template(
        "{spinner:.cyan} [{elapsed_precise}] [{bar:36.cyan/blue}] {bytes}/{total_bytes} N-Quads loaded",
    )?);
    progress.enable_steady_tick(Duration::from_millis(120));
    let mut input = progress.wrap_read(BufReader::new(input));
    data.merge_from_reader(
        &mut input,
        &graph.display().to_string(),
        &RDFFormat::NQuads,
        None,
        &ReaderMode::Strict,
    )
    .with_context(|| format!("parsing {} as N-Quads", graph.display()))?;
    progress.finish();
    let shapes = RdfData::from_str(SHAPES, &RDFFormat::Turtle, None, &ReaderMode::Strict)?;
    let schema: IRSchema = ShaclParser::new(shapes).parse()?.try_into()?;
    let mut validator: DataValidation = data.into();
    let checking = ProgressBar::new_spinner();
    checking.set_style(ProgressStyle::with_template(
        "{spinner:.cyan} [{elapsed_precise}] checking SHACL constraints",
    )?);
    checking.enable_steady_tick(Duration::from_millis(120));
    let report = validator.validate(&schema, &ShaclValidationMode::Native, &ShaclConfig::default())?;
    checking.finish_and_clear();
    if report.conforms() {
        println!("SHACL conforms");
    } else {
        println!("{report}");
    }
    ensure!(report.conforms(), "SHACL validation failed");
    Ok(())
}
