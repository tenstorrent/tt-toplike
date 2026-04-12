extern crate detect_desktop_environment;

use detect_desktop_environment::DesktopEnvironment;

fn main() {
  let de: DesktopEnvironment = DesktopEnvironment::detect();
  println!("{:?}", de);
}
