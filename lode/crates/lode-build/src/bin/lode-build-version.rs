//! Prints the build version, so the release workflow can check it against the tag and hand it
//! to builds that run without Git (the `cross` containers).

fn main() {
    println!("{}", lode_build::VERSION);
}
