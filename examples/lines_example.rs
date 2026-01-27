// Copyright (c) 2026 CyberNestSticks LLC
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

// Author: Lawrence (Larry) Foard

use arcstr::literal;
use zcstring::ZCString;

fn main() {
    let animals: ZCString = literal!(
        r#"
        cats
        dogs
        frogs
    "#
    )
    .into();

    animals
        // wrap an iterator returning method such that it returns
        // an iterator of ZCStrings
        .wrap_iter(|s| s.lines())
        // trim the entry, ZCString::map() converts the resulting
        // &str to a zero-copy ZCString when possible
        .map(|l| l.map(|s| s.trim()))
        // filter empty lines
        .filter(|l| !l.is_empty())
        .for_each(|l| println!("{:?} zero-copy: {}", l, animals.source_of(&l)));
}
