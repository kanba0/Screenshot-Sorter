mod anilist;
mod app;
mod matching;
mod parse;
mod tui;

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(name = "ssort", about = "Sort anime screenshots into series folders")]
struct Args {
    /// Directory containing unsorted screenshots (default: current directory)
    #[arg(short, long)]
    source: Option<PathBuf>,

    /// Directory containing series folders (default: same as source)
    #[arg(short, long)]
    dest: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let source = args.source
        .unwrap_or_else(|| std::env::current_dir().expect("cannot determine current directory"));
    let dest = args.dest.unwrap_or_else(|| source.clone());

    eprintln!("scanning {}...", source.display());
    let screenshots = parse::find_screenshots(&source);

    if screenshots.is_empty() {
        eprintln!("no MPV screenshots found.");
        return Ok(());
    }
    eprintln!("found {} screenshot(s).", screenshots.len());

    eprintln!("building folder tree from {}...", dest.display());
    let tree = matching::build_folder_tree(&dest);
    eprintln!("found {} destination folder(s).", tree.entries.len());

    eprintln!("matching...");
    let mut anilist = anilist::AniListClient::new();
    let entries = matching::run_pipeline(screenshots, &tree, &mut anilist)?;

    eprintln!(
        "done. {} matched, {} need new folders, {} unresolved.\n",
        entries.iter().filter(|e| matches!(e.destination, matching::Destination::Existing(_))).count(),
        entries.iter().filter(|e| matches!(e.destination, matching::Destination::New(_))).count(),
        entries.iter().filter(|e| matches!(e.destination, matching::Destination::Unresolved)).count(),
    );

    tui::run(entries, dest)?;

    Ok(())
}
