mod cases;
mod harness;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("--list") => {
            for case in cases::CASES {
                println!("{}", case.name);
            }
        }
        Some(name) => run_one(name),
        None => {
            let mut failed = 0;
            for case in cases::CASES {
                match cases::run(case.name) {
                    Ok(()) => println!("PASS {}", case.name),
                    Err(error) => {
                        eprintln!("FAIL {}: {error}", case.name);
                        failed += 1;
                    }
                }
            }
            if failed > 0 {
                std::process::exit(1);
            }
        }
    }
}

fn run_one(name: &str) {
    match cases::run(name) {
        Ok(()) => println!("PASS {name}"),
        Err(error) => {
            eprintln!("FAIL {name}: {error}");
            std::process::exit(1);
        }
    }
}
