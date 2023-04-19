use walkdir::WalkDir;

use std::collections::HashMap;
use std::env;
use std::path::Path;

fn main() {
    let args: Vec<_> = env::args().collect();

    let path = args.get(1).expect("usage: genfixture [PATH]");
    let path = Path::new(path);

    let mut tests: HashMap<String, Vec<String>> = HashMap::new();

    for entry in WalkDir::new(path) {
        let entry = entry.unwrap();
        let path = entry.path().to_str().unwrap();

        if !path.ends_with(".json") {
            continue;
        }

        if path.contains("raw-data") {
            continue;
        }

        // Get the relevant part
        let fixture_path = path.split("fixtures/hpack/").last().unwrap();

        // Now, split that into the group and the name
        let module = fixture_path.split('/').next().unwrap();

        tests
            .entry(module.to_string())
            .or_default()
            .push(fixture_path.to_string());
    }

    let mut one = false;

    for (module, tests) in tests {
        let module = module.replace('-', "_");

        if one {
            println!();
        }

        one = true;

        println!("fixture_mod!(");
        println!("    {} => {{", module);

        for test in tests {
            let ident = test.split('/').nth(1).unwrap().split('.').next().unwrap();

            println!("        ({}, {:?});", ident, test);
        }

        println!("    }}");
        println!(");");
    }
}
