// Copyright (c) 2026 CyberNestSticks LLC
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

// Author: Lawrence (Larry) Foard

use arcstr::literal;
#[cfg(feature = "serde_json")]
use serde::{Deserialize, Serialize};
use std::error::Error;

#[cfg(feature = "serde_json")]
use zcstring::serde_json_from_zcstring;
use zcstring::ZCString;

// parse JSON containing borrowed pointers to ParsedLog::owner
// and/or owned de-escapified data
#[derive(Debug)]
#[cfg_attr(feature = "serde_json", derive(Deserialize, Serialize))]
struct LogEntry {
    level: ZCString,
    message: ZCString,
}

fn show(label: &str, source: &str, s: &str) {
    println!("  Field: {}", label);
    println!("    Value: {:?}", s);

    // memory position of s
    let s_start = s.as_ptr() as usize;
    println!("    Address: 0x{:x}", s_start);

    // bounds of source
    let source_start = source.as_ptr() as usize;
    let source_end = source_start + source.len();

    if s_start >= source_start && s_start < source_end {
        println!("    Value falls within source");
    } else {
        println!("    Value doesn't fall within source");
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    #[cfg(feature = "serde_json")]
    {
        // example JSON data feed
        let input = [
            literal!(r#"{"level": "error", "message": "Connection lost"}"#),
            literal!(r#"{"level": "warning", "message": "Cat on keyboard"}"#),
            literal!(r#"{"level": "info", "message": "Crow pecked camera"}"#),
            literal!(r#"{"level": "error", "message": "Raven pecked camera, now offline"}"#),
            // in this case the address of message should not fall within
            // the memory address range of the raw json
            literal!(r#"{"level": "error", "message": "Escaped \" "}"#),
        ];

        let items = input
            .into_iter()
            .map(|line| -> Result<LogEntry, Box<dyn Error>> {
                // our special wrapper for JSON parsing
                let entry = serde_json_from_zcstring::<LogEntry>(ZCString::from(line.clone()))?;

                // show values and memory layout
                println!("------");

                println!("Log Line: {}", line);
                println!(
                    "  Log Line Location: 0x{:x} - 0x{:x}",
                    line.as_ptr() as usize,
                    line.as_ptr() as usize + line.len(),
                );

                show("level", &line, &entry.level);
                show("message", &line, &entry.message);

                // now serialize - Ok we could do a zero-alloc deserialize but
                //                 not right now...
                println!("  Serialized: {}", serde_json::to_string(&entry)?);
                println!("");

                Ok(entry)
            })
            .collect::<Result<Vec<LogEntry>, _>>()?;

        println!("items size: {}\n", items.len());
    }

    Ok(())
}
