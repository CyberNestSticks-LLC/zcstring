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
    // ZCString creation examples
    println!("From str: {:?}", ZCString::from("str"));
    #[cfg(feature = "std")]
    println!("From String: {:?}", ZCString::from(String::from("str")));
    #[cfg(feature = "std")]
    println!("String::from(\"a\") == ZCString::from(\"a\"): {:?}", 
        String::from("a") == ZCString::from("a"));
    println!("New ZCString: {:?}", ZCString::new());

    // how big is a ZCString member in a structure as compared &str?

    // we expect the same size. Why? &str is a fat pointer and
    // ZString is a Substr which is a thin pointer to an ArcStr plus
    // a range consisting of two u32s
    println!("size_of &str: {}", size_of::<&str>());
    println!("size_of ZCString: {}", size_of::<ZCString>());

    // create a ZCString pointing to a staticly defined &str
    let zc = ZCString::from(literal!("cats and dogs"));

    // lets make some substrings
    let s1 = zc.substr(0..4);
    let s2 = zc.substr(9..12);

    // show the strings and the fact they live in zc
    println!("s1: {:?} lives in zc? {}", s1, zc.source_of(&s1));
    println!("s2: {:?} lives in zc? {}", s2, zc.source_of(&s2));
}
