use std::process::{Command, Output};

/// Sets a boolean value through dconf
pub fn set_boolean(key: &str, value: bool) -> Result<(), String> {
	set(key, &format!("{}", value))
}

/// Gets a boolean value through dconf
pub fn get_boolean(key: &str) -> Result<bool, String> {
	match get(key) {
		Ok(value) => Ok(value == "true"),
		Err(why) => Err(why)
	}
}

/// Sets a string value through dconf
pub fn set_string(key: &str, value: &str) -> Result<(), String> {
    set(key, &format!("'{}'", value))
}

/// Gets a string value through dconf
pub fn get_string(key: &str) -> Result<String, String> {
	get(key)
}

/// Sets an int value through dconf
pub fn set_int(key: &str, value: i32) -> Result<(), String> {
    set(key, &format!("{}", value))
}

/// Gets an int value through dconf
pub fn get_int(key: &str) -> Result<i32, String> {
	match get(key) {
		Ok(value) => {
			let conversion = value.parse::<i32>();
			match conversion {
				Ok(num) => Ok(num),
				Err(_) => Err("Value is not an integer".to_string())
			}
		},
		Err(why) => Err(why)
	}
}

/// Sets a uint value through dconf
pub fn set_uint(key: &str, value: u32) -> Result<(), String> {
    set(key, &format!("{}", value))
}

/// Gets a uint value through dconf
pub fn get_uint(key: &str) -> Result<u32, String> {
	match get(key) {
		Ok(value) => {
			let conversion = value.parse::<u32>();
			match conversion {
				Ok(num) => Ok(num),
				Err(_) => Err("Value is not an integer".to_string())
			}
		},
		Err(why) => Err(why)
	}
}

/// Sets a double value through dconf
pub fn set_double(key: &str, value: f64) -> Result<(), String> {
    set(key, &format!("{}", value))
}

/// Gets a double value through dconf
pub fn get_double(key: &str) -> Result<f64, String> {
	match get(key) {
		Ok(value) => {
			let conversion = value.parse::<f64>();
			match conversion {
				Ok(num) => Ok(num),
				Err(_) => Err("Value is not a double".to_string())
			}
		},
		Err(why) => Err(why)
	}
}


// Helpers
fn get(key: &str) -> Result<String, String> {
    let mut cmd = Command::new("dconf");
	cmd.args(&["read", key]);
	match cmd.output() {
		Ok(output) => Ok(get_stdout(output)),
		Err(_) => Err("Unable to get key".to_string()),
	}
}

fn set(key: &str, value: &str) -> Result<(), String> {
    let mut cmd = Command::new("dconf");
	cmd.args(&["write", key, value]);
	match cmd.output() {
		Ok(_) => Ok(()),
		Err(_) => Err("Unable to set key".to_string()),
	}
}

fn get_stdout(output: Output) -> String {
	let vs = output.stdout;
	String::from_utf8(vs).unwrap().replace("\'", "").replace("\n", "")
}
