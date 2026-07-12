use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "leadsheet", version, about = "MIDI ↔ compact semantic text")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Args)]
struct TempoArgs {
    /// Infer tempo from onsets even if the file declares one.
    #[arg(long)]
    infer_tempo: bool,
    /// Trust the declared tempo unconditionally (no auto-switch).
    #[arg(long, conflicts_with = "infer_tempo")]
    no_infer_tempo: bool,
    /// Force this BPM (phase/downbeat still estimated).
    #[arg(long)]
    bpm: Option<f64>,
}

impl TempoArgs {
    fn options(&self) -> leadsheet_core::grid::QuantizeOptions {
        leadsheet_core::grid::QuantizeOptions {
            bpm_override: self.bpm,
            infer_tempo: self.infer_tempo,
            no_infer: self.no_infer_tempo,
        }
    }
}

fn tempo_notice(report: &leadsheet_core::grid::QuantizeReport) {
    if let leadsheet_core::grid::TempoSource::AutoInferred { declared_bpm, declared_mean_ms } =
        report.tempo_source
    {
        eprintln!(
            "note: declared {declared_bpm:.2} BPM fits poorly (mean {declared_mean_ms:.0} ms off-grid); \
             using inferred {:.2} BPM (mean {:.0} ms). --no-infer-tempo to override",
            report.bpm, report.mean_abs_residual_ms
        );
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Ingest a .mid or MuScriptor .jsonl and print what was understood,
    /// including the tempo/grid the compressor would use.
    Inspect {
        input: PathBuf,
        #[command(flatten)]
        tempo: TempoArgs,
    },
    /// Compress .mid / MuScriptor .jsonl into leadsheet text.
    Compress {
        input: PathBuf,
        /// Output path (default: input with .ls extension; `-` for stdout).
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[command(flatten)]
        tempo: TempoArgs,
    },
    /// Render leadsheet text back to a standard MIDI file.
    Render {
        input: PathBuf,
        /// Output path (default: input with .mid extension).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// compress → render → re-ingest → note F1 + compression ratio.
    Roundtrip {
        input: PathBuf,
        /// Also write the intermediate .ls text here.
        #[arg(long)]
        keep_text: Option<PathBuf>,
        #[command(flatten)]
        tempo: TempoArgs,
    },
    /// Parse and validate leadsheet text; print diagnostics. Exit 0 only
    /// when the file is valid.
    Check {
        input: PathBuf,
        /// Machine-readable output (one JSON object).
        #[arg(long)]
        json: bool,
    },
    /// Rewrite leadsheet text in canonical form. Document-canonical:
    /// hand-authored structure (pattern ids, multi-bar patterns, direct
    /// bars, labels) survives — never reinterprets a note.
    Fmt {
        input: PathBuf,
        /// Output path (default: rewrite in place; `-` for stdout).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Compress { input, output, tempo } => {
            let song = leadsheet_core::ingest::ingest_path(&input)?;
            let (qsong, report) = leadsheet_core::grid::quantize(&song, &tempo.options());
            tempo_notice(&report);
            let text = leadsheet_core::emit::emit(&qsong);
            let naive = leadsheet_core::metrics::naive_event_text(&song).len();
            let out_path = output.unwrap_or_else(|| input.with_extension("ls"));
            if out_path.as_os_str() == "-" {
                print!("{text}");
            } else {
                std::fs::write(&out_path, &text)?;
                eprintln!("wrote {}", out_path.display());
            }
            eprintln!(
                "tempo {:.2} BPM ({:?}), {} bars, {} notes; {} bytes ({:.1}x vs naive event list)",
                report.bpm,
                report.tempo_source,
                qsong.n_bars,
                report.note_count,
                text.len(),
                naive as f64 / text.len().max(1) as f64,
            );
        }
        Cmd::Render { input, output } => {
            let text = std::fs::read_to_string(&input)?;
            let qsong = leadsheet_core::parse::parse(&text)?;
            let bytes = leadsheet_core::render::render(&qsong);
            let out_path = output.unwrap_or_else(|| input.with_extension("mid"));
            std::fs::write(&out_path, &bytes)?;
            eprintln!(
                "wrote {} ({:.2} BPM, {} bars, {} tracks)",
                out_path.display(),
                qsong.bpm,
                qsong.n_bars,
                qsong.tracks.len()
            );
        }
        Cmd::Roundtrip { input, keep_text, tempo } => {
            let song = leadsheet_core::ingest::ingest_path(&input)?;
            let report = leadsheet_core::metrics::roundtrip(&song, &tempo.options())?;
            tempo_notice(&report.quant);
            if let Some(path) = keep_text {
                std::fs::write(&path, &report.text)?;
                eprintln!("wrote {}", path.display());
            }
            println!(
                "tempo     {:.2} BPM ({:?}), origin {:+.3} s",
                report.quant.bpm, report.quant.tempo_source, report.quant.origin
            );
            println!(
                "notes     {} in, {} out, {} matched",
                report.f1.ref_count, report.f1.hyp_count, report.f1.matched
            );
            println!(
                "F1        {:.4}  (precision {:.4}, recall {:.4})",
                report.f1.f1(),
                report.f1.precision(),
                report.f1.recall()
            );
            println!(
                "size      {} bytes text vs {} naive ({:.1}x)",
                report.ls_bytes(),
                report.naive_bytes,
                report.ratio_vs_naive()
            );
            if report.f1.f1() < 0.95 {
                eprintln!("WARN: F1 below 0.95 target");
                std::process::exit(1);
            }
        }
        Cmd::Inspect { input, tempo } => {
            let song = leadsheet_core::ingest::ingest_path(&input)?;
            let (infer_tempo, bpm) = (tempo.infer_tempo, tempo.bpm);
            println!("song: {}", song.name);
            match song.source_bpm {
                Some(bpm) => println!("source tempo: {bpm:.2} BPM (declared, constant)"),
                None => println!("source tempo: none declared (will be inferred)"),
            }
            println!("duration: {:.2} s, {} notes", song.duration(), song.note_count());
            for t in &song.tracks {
                let lo = t.notes.iter().map(|n| n.pitch).min().unwrap_or(0);
                let hi = t.notes.iter().map(|n| n.pitch).max().unwrap_or(0);
                println!(
                    "  {:<20} {:>5} notes  pitch {}..{}  {}",
                    t.name,
                    t.notes.len(),
                    lo,
                    hi,
                    if t.is_drums {
                        "drums".to_string()
                    } else {
                        format!(
                            "program {} ({})",
                            t.program,
                            leadsheet_core::gm::program_name(t.program)
                        )
                    }
                );
            }
            let opts = leadsheet_core::grid::QuantizeOptions {
                bpm_override: bpm,
                infer_tempo,
                ..Default::default()
            };
            let (qsong, report) = leadsheet_core::grid::quantize(&song, &opts);
            println!(
                "grid: {:.2} BPM ({:?}), origin {:+.3} s, {} bars of {}/{}, key {}",
                report.bpm,
                report.tempo_source,
                report.origin,
                qsong.n_bars,
                qsong.meter.0,
                qsong.meter.1,
                qsong.key.map(|k| k.name()).unwrap_or_else(|| "?".into()),
            );
            println!(
                "µtiming discarded by 1/16 snap: mean {:.1} ms, max {:.1} ms",
                report.mean_abs_residual_ms, report.max_abs_residual_ms
            );
        }
        Cmd::Check { input, json } => {
            let read = std::fs::read_to_string(&input);
            let outcome = read
                .map_err(anyhow::Error::from)
                .and_then(|text| leadsheet_core::parse::parse(&text).map_err(anyhow::Error::from));
            match outcome {
                Ok(q) => {
                    let notes: usize = q.tracks.iter().map(|t| t.notes.len()).sum();
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "ok": true,
                                "bars": q.n_bars,
                                "tracks": q.tracks.len(),
                                "notes": notes,
                            })
                        );
                    } else {
                        println!(
                            "ok: {} bars, {} tracks, {notes} notes ({:.2} BPM, {}/{})",
                            q.n_bars,
                            q.tracks.len(),
                            q.bpm,
                            q.meter.0,
                            q.meter.1
                        );
                    }
                }
                Err(e) => {
                    if json {
                        let payload = match e
                            .downcast_ref::<leadsheet_core::Error>()
                            .and_then(|e| e.diagnostic())
                        {
                            Some(d) => serde_json::json!({ "ok": false, "diagnostics": [d] }),
                            None => serde_json::json!({ "ok": false, "error": e.to_string() }),
                        };
                        println!("{payload}");
                    } else {
                        eprintln!("{e}");
                    }
                    std::process::exit(1);
                }
            }
        }
        Cmd::Fmt { input, output } => {
            let text = std::fs::read_to_string(&input)?;
            let document = leadsheet_core::parse::parse_document(&text)?;
            let qsong = document.resolve()?;
            let canonical = leadsheet_core::emit::emit_document(&document);
            let out_path = output.unwrap_or_else(|| input.clone());
            if out_path.as_os_str() == "-" {
                print!("{canonical}");
            } else {
                let unchanged = canonical == text;
                std::fs::write(&out_path, &canonical)?;
                eprintln!(
                    "{} {} ({} bars{})",
                    if unchanged { "unchanged" } else { "formatted" },
                    out_path.display(),
                    qsong.n_bars,
                    if unchanged { "" } else { ", canonical form" },
                );
            }
        }
    }
    Ok(())
}
