#[path = "../cratonctl/mod.rs"]
mod cratonctl;

fn main() {
    std::process::exit(cratonctl::run(std::env::args().collect()));
}
