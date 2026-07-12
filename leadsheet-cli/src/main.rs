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
    /// Ingest a .mid or MuScriptor .jsonl and print what was understood.
    Inspect { input: PathBuf },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Inspect { input } => {
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
        }
    }
    Ok(())
}
