//! Export canonical Rust DTO schemas without opening a broker or journal.

use devagotchi_contracts::{
    BridgeError, FileContent, FileEditInput, FileListInput, FileListing, FileReadInput,
    FileWriteInput, FileWritten, GetInput, ListInput, OperationView, OperationsPage, OutputPage,
    ReadInput, StartInput, StartOutput,
};
use schemars::{JsonSchema, generate::SchemaSettings};
use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;

/// Export-only schema root. These fields reference canonical Rust DTOs and are
/// not a broker request or a second set of independently authored contracts.
#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(deny_unknown_fields)]
struct BrokerContracts {
    start_input: StartInput,
    get_input: GetInput,
    read_input: ReadInput,
    list_input: ListInput,
    start_output: StartOutput,
    operation_view: OperationView,
    output_page: OutputPage,
    operations_page: OperationsPage,
    file_read_input: FileReadInput,
    file_content: FileContent,
    file_list_input: FileListInput,
    file_listing: FileListing,
    file_write_input: FileWriteInput,
    file_edit_input: FileEditInput,
    file_written: FileWritten,
    bridge_error: BridgeError,
}

/// Sort every JSON object so output stays deterministic even if another crate
/// enables serde_json's preserve_order feature. Array order remains meaningful.
fn canonical_json(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries: Vec<_> = object.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonical_json(value)))
                    .collect(),
            )
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonical_json).collect()),
        scalar => scalar,
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let output = match arguments.next() {
        None => None,
        Some(flag) if flag == "--output" => {
            let path = arguments.next().ok_or("--output requires a file path")?;
            if arguments.next().is_some() {
                return Err("Unexpected argument after output path".into());
            }
            Some(PathBuf::from(path))
        }
        Some(flag) if flag == "--help" || flag == "-h" => {
            println!(
                "Usage: export-contracts [--output FILE]\nExports canonical JSON Schema to stdout unless an output file is explicitly requested."
            );
            return Ok(());
        }
        Some(_) => return Err("Usage: export-contracts [--output FILE]".into()),
    };
    // A single generator/root allows Schemars to deduplicate all shared $defs.
    // The schema describes Serde's DTO deserialization contract, including
    // optional/defaulted request fields; broker domain bounds remain runtime checks.
    let schema = SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<BrokerContracts>();
    let value = canonical_json(serde_json::to_value(schema)?);
    let mut bytes = serde_json::to_vec_pretty(&value)?;
    bytes.push(b'\n');
    match output {
        None => std::io::stdout().lock().write_all(&bytes)?,
        Some(path) => {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, bytes)?;
        }
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Contract export failed: {error}");
        std::process::exit(1);
    }
}
