use clap::{Parser, ValueEnum};

#[derive(Parser, Debug)]
#[command(name = "gest", version, about = "Jest-like Go test runner")]
pub struct Cli {
    #[arg(long, value_enum, default_value = "all")]
    pub mode: ModeArg,
    #[arg(long, default_value_t = num_cpus::get())]
    pub pkg_concurrency: usize,
    #[arg(long)]
    pub sequential: bool,
    #[arg(long)]
    pub no_watch: bool,
    #[arg(long)]
    pub no_test_cache: bool,
    #[arg(long)]
    pub packages: Option<String>,
    #[arg(long)]
    pub debug: bool,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum ModeArg {
    All,
    Failing,
    Select,
}
