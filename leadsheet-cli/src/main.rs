use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "leadsheet", version, about = "MIDI ↔ compact semantic text")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Ingest a .mid or MuScriptor .jsonl and print what was understood,
    /// including the tempo/grid the compressor would use.
    Inspect {
        input: PathBuf,
        /// Infer tempo from onsets even if the file declares one.
        #[arg(long)]
        infer_tempo: bool,
        /// Force this BPM (phase/downbeat still estimated).
        #[arg(long)]
        bpm: Option<f64>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Inspect { input, infer_tempo, bpm } => {
            let song = leadsheet_core::ingest::ingest_path(&input)?;
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
                        format!("program {} ({})", t.program, leadsheet_core::gm::program_name(t.program))
                    }
                );
            }
            let opts = leadsheet_core::grid::QuantizeOptions { bpm_override: bpm, infer_tempo };
            let (qsong, report) = leadsheet_core::grid::quantize(&song, &opts);
            println!(
                "grid: {:.2} BPM ({:?}), origin {:+.3} s, {} bars of {}/{}",
                report.bpm,
                report.tempo_source,
                report.origin,
                qsong.n_bars,
                qsong.meter.0,
                qsong.meter.1,
            );
            println!(
                "µtiming discarded by 1/16 snap: mean {:.1} ms, max {:.1} ms",
                report.mean_abs_residual_ms, report.max_abs_residual_ms
            );
        }
    }
    Ok(())
}
