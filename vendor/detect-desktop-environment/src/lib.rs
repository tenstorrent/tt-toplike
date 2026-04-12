use ::std::env;

#[derive(Debug, PartialEq, Eq)]
pub enum DesktopEnvironment {
  Unknown,
  Cinnamon,
  Enlightenment,
  Gnome,
  Kde,
  Lxde,
  Lxqt,
  MacOs,
  Mate,
  Unity,
  Windows,
  Xfce,
}

impl DesktopEnvironment {
  #[cfg(target_os = "macos")]
  pub fn detect() -> Self {
    DesktopEnvironment::MacOs
  }

  #[cfg(target_os = "windows")]
  pub fn detect() -> Self {
    DesktopEnvironment::Windows
  }

  #[cfg(not(any(target_os = "macos", target_os = "windows")))]
  pub fn detect() -> Self {
    match env::var("XDG_CURRENT_DESKTOP") {
      Err(_) => DesktopEnvironment::Unknown,
      Ok(current_desktop) => {
        match current_desktop.as_str() {
          "Cinnamon" => DesktopEnvironment::Cinnamon,
          "ENLIGHTENMENT" => DesktopEnvironment::Enlightenment,
          "GNOME" => DesktopEnvironment::Gnome,
          "KDE" => DesktopEnvironment::Kde,
          "LXDE" => DesktopEnvironment::Lxde,
          "LXQt" => DesktopEnvironment::Lxqt,
          "MATE" => DesktopEnvironment::Mate,
          "Unity" => DesktopEnvironment::Unity,
          "X-Cinnamon" => DesktopEnvironment::Cinnamon,
          "XFCE" => DesktopEnvironment::Xfce,
          _ => DesktopEnvironment::Unknown,
        }
      }
    }
  }
}
