// Copyright (c) 2026 CyberNestSticks LLC
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

// Author: Lawrence (Larry) Foard
#[cfg(all(feature = "serde_json", feature = "std"))]
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use zcstring::{serde_json_from_zcstring, ZCString};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub enum Status {
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "inactive")]
    Inactive,
    #[serde(rename = "maintenance")]
    Maintenance,
    #[serde(rename = "warning")]
    Warning,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct State {
    population: u64,
    state_bird: ZCString,
    capital: ZCString,
    sensor_id: ZCString,
    temp_c: f64,
    status: Status,
}

#[derive(Debug, Deserialize, Serialize)]
struct Country {
    states: HashMap<ZCString, State>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Construct a path relative to the project root
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("examples");
    // dummy test data
    path.push("file_example.json");

    let data = ZCString::from_file(path)?;
    let country: Country = serde_json_from_zcstring(data)?;

    let active: HashMap<_, _> = country
        .states
        .iter()
        .filter(|(_, data)| data.status == Status::Active)
        // k.clone() is zero-copy and still points back to 'data'
        // as do CZStrings within State
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    println!("{}", serde_json::to_string_pretty(&active)?);
    Ok(())
}
