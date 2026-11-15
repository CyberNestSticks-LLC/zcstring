// Copyright (c) 2026 CyberNestSticks LLC
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

// Author: Lawrence (Larry) Foard

//! # ZCString
//!
//! `ZCString` is a context-aware string handle wrapping [`arcstr::Substr`], designed
//! for efficient, zero-copy string manipulation and deserialization.
//!
//! It uses thread-local storage to track an active "source" string buffer. When substrings
//! are parsed or derived from that buffer, they can be created as zero-allocation sub-slices
//! referencing the original buffer rather than duplicating string contents into new heap
//! allocations.
//!
//! ## Core Features
//!
//! - **Context-Aware Slicing**: Maintains an active `SOURCE` string in thread-local storage.
//!   When creating a `ZCString` from a `&str` (via [`ZCString::from`] or deserialization),
//!   it checks if the string slice resides within the active source buffer. If so, it returns
//!   a zero-copy slice; otherwise, it falls back to a standard allocation.
//! - **RAII Scope Guards**: Provides [`SourceGuard`] and [`ZCString::with_source`] to safely
//!   scope the lifetime of the active source, supporting arbitrary nesting with automatic
//!   restoration of outer sources upon scope exit.
//! - **Serde Integration**: Provides [`serde_json_from_zcstring`] for zero-copy deserialization
//!   of JSON strings. Unescaped strings become zero-copy slices pointing directly into the
//!   JSON payload, while escaped strings (which require unescaping) fall back to allocated strings.
//! - **Raw and Lazy JSON Values**:
//!   - [`ZCRawValue`]: Preserves unparsed JSON fragments as zero-copy raw string slices.
//!   - [`ZCRawValueAnd`]: Holds both the unparsed raw JSON slice and the eagerly parsed Rust value `T`.
//!   - [`ZCLazyValue`]: Defers parsing of a raw JSON slice into `T` until first access,
//!     caching the result in a thread-safe [`std::sync::OnceLock`].
//! - **Stream and File Reading**: Utility functions [`ZCString::read_range`], [`ZCString::read_exact`],
//!   and [`ZCString::from_file`] allow efficient loading of UTF-8 string data from files and byte streams.
//! - **Thread Safety**: `ZCString` implements [`Send`] and [`Sync`]. The thread-local source context
//!   is strictly thread-isolated; [`SourceGuard`] is `!Send` and `!Sync` to guarantee scope stack integrity.
//!
//! ## Serde JSON Example
//!
//! ```rust
//! use arcstr::literal;
//! use serde::Deserialize;
//! use std::error::Error;
//! use zcstring::{serde_json_from_zcstring, ZCString};
//!
//! #[derive(Debug, Deserialize)]
//! struct Animal {
//!     animal: ZCString,
//!     color: ZCString,
//! }
//!
//! fn main() -> Result<(), Box<dyn Error>> {
//!     let json = literal!(r#"{"animal":"cat", "color": "red"}"#);
//!     // serde_json_from_zcstring sets `json` as the active thread-local source.
//!     // When done, struct members point directly to memory within the `json` string.
//!     let animal = serde_json_from_zcstring::<Animal>(json.into())?;
//!     println!("animal: {:?}", animal);
//!     Ok(())
//! }
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]

use arcstr::{ArcStr, Substr};

// re-export
pub use arcstr::{literal, literal_substr};
use log::error;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde::de::{DeserializeOwned};
use serde::ser::Error;
use serde_json::value::RawValue;
use core::cell::RefCell;
use std::cmp::Ordering;
use std::fmt::{self, Debug};
use std::hash::{Hash, Hasher};
use std::io::{Read, Seek, SeekFrom};
use std::marker::PhantomData;
use std::ops::Deref;
use core::ops::{Bound, RangeBounds};
use std::sync::{Arc, OnceLock};

thread_local! {
    /// The thread-local storage holding the current active source string.
    static SOURCE: RefCell<Option<ZCString>> =
        const { RefCell::new(None) };
}

/// Errors that can occur during file reading, streaming, or range operations.
#[derive(thiserror::Error, Debug)]
pub enum ReaderError {
    /// The requested byte range is invalid (e.g., start > end, end exceeds stream length,
    /// or range bounds exceed addressable integer limits).
    #[error("Invalid range: start {start}, end {end}")]
    InvalidRange {
        /// Start byte offset of the invalid range.
        start: u64,
        /// End byte offset of the invalid range.
        end: u64,
    },

    /// An I/O error occurred while reading from or seeking within the underlying stream.
    #[error("IO failure: {0}")]
    Io(#[from] std::io::Error),

    /// The read byte slice contains invalid UTF-8 sequences.
    #[error("UTF-8 encoding failure: {0}")]
    Utf8(#[from] std::str::Utf8Error),
}

/// A context-aware string handle for zero-copy string slicing and deserialization.
///
/// `ZCString` wraps [`arcstr::Substr`], providing a reference-counted, zero-allocation
/// substring type that shares its underlying string buffer with an [`arcstr::ArcStr`].
///
/// # Key Capabilities
///
/// - **Zero-Copy Substrings**: Substrings created via [`substr`](Self::substr),
///   [`from_substr`](Self::from_substr), or [`map`](Self::map) point directly into the
///   parent buffer without re-allocating.
/// - **Thread-Local Context**: Using [`with_source`](Self::with_source) or
///   [`get_source_guard`](Self::get_source_guard), a `ZCString` can be registered as the
///   active source in thread-local storage. Any subsequent conversions via
///   [`From<&str>`](#impl-From<&str>-for-ZCString) or Serde deserialization will inspect
///   the active source and borrow slices from it if the string resides in that buffer.
/// - **Serde Integration**: Automatically integrates with `serde_json` through
///   [`serde_json_from_zcstring`], creating zero-copy string fields for unescaped JSON strings.
/// - **Thread Safety**: `ZCString` implements [`Send`] and [`Sync`]. The thread-local context
///   operates independently per thread.
/// - **Compact Size**: Sized identically to `&str` (16 bytes on 64-bit systems: a pointer
///   and two 32-bit offset values).
#[derive(Clone, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ZCString(Substr);

impl ZCString {
    /// Creates a new, empty `ZCString`.
    ///
    /// This is a `const fn` that performs no heap allocations.
    ///
    /// # Example
    /// ```
    /// use zcstring::ZCString;
    ///
    /// let empty = ZCString::new();
    /// assert!(empty.is_empty());
    /// assert_eq!(empty, "");
    /// ```
    pub const fn new() -> Self {
        ZCString(literal_substr!(""))
    }

    /// Shorthand constructor to wrap a compile-time static [`Substr`].
    ///
    /// This creates a `ZCString` backed by static memory with zero heap allocations.
    ///
    /// # Example
    /// ```
    /// use arcstr::literal_substr;
    /// use zcstring::ZCString;
    ///
    /// const GREETING: ZCString = ZCString::l(literal_substr!("hello"));
    /// assert_eq!(GREETING, "hello");
    /// ```
    pub const fn l(v: Substr) -> Self {
        ZCString(v)
    }

    /// Create an independent allocated copy of the underlying string
    /// buffer detached from the original string buffer.
    ///
    /// This is useful when this `ZCString` is a small sub-slice of a much larger buffer
    /// (e.g. a 2-byte substring of a 1GB buffer) and you want to allow the larger
    /// buffer to be deallocated while retaining only the small slice.
    ///
    /// # Example
    /// ```
    /// use zcstring::ZCString;
    /// use arcstr::literal_substr;
    ///
    /// const EMPTY: ZCString = ZCString::new();
    /// const X: ZCString = ZCString::l(literal_substr!("x"));
    /// let large_source = ZCString::from_str_without_source("..."); // 1GB string
    /// let small_slice = large_source.substr(0..2);
    ///
    /// // Detach from the 1GB buffer to allow it to be garbage collected
    /// let owned_slice = small_slice.detach();
    /// ```
    pub fn detach(&self) -> Self {
        // create a new allocation
        ZCString::from_str_without_source(self.as_str())
    }

    /// Returns `true` if the string slice `s` physically resides within the
    /// memory bounds of this `ZCString`.
    ///
    /// This checks whether the pointer range `[s.as_ptr(), s.as_ptr() + s.len()]`
    /// falls completely inside `[self.as_ptr(), self.as_ptr() + self.len()]`.
    /// Checked pointer arithmetic is used to prevent integer overflow.
    ///
    /// # Example
    /// ```
    /// use zcstring::ZCString;
    ///
    /// let root = ZCString::from_str_without_source("hello world");
    /// let sub = &root[0..5];
    /// assert!(root.source_of(sub));
    ///
    /// let other = "hello";
    /// assert!(!root.source_of(other));
    /// ```
    pub fn source_of(&self, s: &str) -> bool {
        let self_start = self.0.as_ptr() as usize;
        let s_start = s.as_ptr() as usize;
        if let Some(offset) = s_start.checked_sub(self_start) {
            offset.checked_add(s.len()).is_some_and(|end| end <= self.0.len())
        } else {
            false
        }
    }

    /// Creates a [`ZCString`] from a string slice `s`.
    ///
    /// If `s` physically resides within the bounds of this [`ZCString`] (as verified
    /// by [`source_of`](Self::source_of)), this returns a zero-allocation sub-slice
    /// referencing the existing buffer. If `s` does not reside within this string,
    /// it falls back to allocating an independent buffer via
    /// [`from_str_without_source`](Self::from_str_without_source).
    ///
    /// # Arguments
    /// * `s` - The string slice to convert into a `ZCString`.
    ///
    /// # Returns
    /// A zero-copy `ZCString` borrowing from `self` if `s` is a sub-slice, or a newly allocated
    /// `ZCString` if `s` is external.
    ///
    /// # Example
    /// ```
    /// use zcstring::ZCString;
    ///
    /// let root = ZCString::from("hello zero-copy world");
    /// let sub = &root[6..15]; // "zero-copy"
    /// let zc_sub = root.from_substr(sub);
    /// assert_eq!(zc_sub, "zero-copy");
    /// assert!(root.source_of(&zc_sub));
    ///
    /// let external = "hello";
    /// let zc_ext = root.from_substr(external);
    /// assert_eq!(zc_ext, "hello");
    /// assert!(!root.source_of(&zc_ext));
    /// ```
    pub fn from_substr(&self, s: &str) -> Self {
        let self_start = self.0.as_ptr() as usize;
        let s_start = s.as_ptr() as usize;
        match s_start.checked_sub(self_start) {
            Some(offset) if offset.checked_add(s.len()).is_some_and(|end| end <= self.0.len()) => {
                self.substr(offset..offset + s.len())
            }
            _ => ZCString::from_str_without_source(s),
        }
    }

    /// Creates a [`ZCString`] by allocating a new [`ArcStr`].
    ///
    /// This bypasses the thread-local source context check and directly creates an
    /// independent heap-allocated buffer. Use this when initializing root source strings,
    /// or when you know the input slice is independent and want to avoid thread-local
    /// lookup overhead.
    ///
    /// # Arguments
    /// * `s` - The string slice to copy into a new `ZCString`.
    ///
    /// # Returns
    /// A new `ZCString` owning its allocated string buffer.
    ///
    /// # Example
    /// ```
    /// use zcstring::ZCString;
    ///
    /// let independent = ZCString::from_str_without_source("independent buffer");
    /// assert_eq!(independent, "independent buffer");
    /// ```
    pub fn from_str_without_source(s: &str) -> Self {
        ZCString(Substr::from(ArcStr::from(s)))
    }

    /// Creates a [`ZCString`] by checking if `s` is a sub-slice of the active
    /// thread-local `SOURCE`.
    ///
    /// If an active source is present and `s` resides within its memory bounds,
    /// a zero-allocation sub-slice is returned. If no source is active or `s` does
    /// not reside in the active source buffer, it falls back to [`Self::from_str_without_source`].
    ///
    /// This is the core contextual constructor called by [`From<&str>`] and Serde deserialization.
    ///
    /// # Arguments
    /// * `s` - The string slice to convert.
    ///
    /// # Returns
    /// A zero-copy `ZCString` if `s` is within the active source, or a newly allocated `ZCString`.
    ///
    /// # Example
    /// ```
    /// use zcstring::ZCString;
    ///
    /// let root = ZCString::from("contextual string");
    /// ZCString::with_source(root.clone(), |src| {
    ///     let slice = &src[0..10]; // "contextual"
    ///     let zc = ZCString::from_str_with_source(slice);
    ///     assert_eq!(zc, "contextual");
    ///     assert!(src.source_of(&zc));
    /// });
    /// ```
    pub fn from_str_with_source(s: &str) -> Self {
        SOURCE.with(|ctx| match ctx.borrow().as_ref() {
            Some(source) => source.from_substr(s),
            None => ZCString::from_str_without_source(s),
        })
    }

    /// Returns a sub-slice of this [`ZCString`] as a new [`ZCString`].
    ///
    /// The resulting `ZCString` shares the same underlying [`arcstr::ArcStr`] buffer
    /// with zero allocations.
    ///
    /// # Arguments
    /// * `range` - Any range bounded within `0..self.len()`.
    ///
    /// # Returns
    /// A new zero-copy `ZCString` sub-slice.
    ///
    /// # Panics
    /// Panics if the range is out of bounds or does not lie on UTF-8 code point boundaries.
    ///
    /// # Example
    /// ```
    /// use zcstring::ZCString;
    ///
    /// let s = ZCString::from("abcdef");
    /// assert_eq!(s.substr(1..4), "bcd");
    /// assert_eq!(s.substr(..3), "abc");
    /// assert_eq!(s.substr(3..), "def");
    /// assert_eq!(s.substr(..), "abcdef");
    /// ```
    pub fn substr(&self, range: impl RangeBounds<usize>) -> Self {
        ZCString(self.0.substr(range))
    }

    /// Returns an RAII [`SourceGuard`] that sets this string as the thread-local
    /// source context.
    ///
    /// When the guard is dropped, the previous source is restored. Multiple guards
    /// can be nested in the same thread; they must be dropped in LIFO order to
    /// preserve the source hierarchy.
    ///
    /// Note: To guarantee thread-local stack integrity, [`SourceGuard`] is `!Send` and `!Sync`.
    ///
    /// # Returns
    /// An RAII guard that manages the lifetime of this source in thread-local storage.
    ///
    /// # Example
    /// ```
    /// use zcstring::ZCString;
    ///
    /// let source = ZCString::from("alpha beta gamma");
    /// {
    ///     let _guard = source.get_source_guard();
    ///     let word = ZCString::from(&source[0..5]);
    ///     assert_eq!(word, "alpha");
    ///     assert!(source.source_of(&word));
    /// }
    /// // Previous source context is restored after guard is dropped.
    /// ```
    #[must_use = "the source guard will restore the previous source immediately if not bound to a variable"]
    pub fn get_source_guard(&self) -> SourceGuard {
        let mut source = Some(self.clone());

        SOURCE.with(|ctx| {
            let mut borrow = ctx.borrow_mut();
            std::mem::swap(&mut *borrow, &mut source);
        });

        SourceGuard {
            old_source: source,
            _marker: PhantomData,
        }
    }

    /// Executes a closure with this `ZCString` set as the thread-local source.
    ///
    /// This is the preferred way to handle contextual string operations.
    ///
    /// # Example
    /// ```
    /// use zcstring::ZCString;
    ///
    /// let source = ZCString::from("1 23 456 789 0");
    ///
    /// // Call a lambda function with our thread local storage
    /// // set to source
    /// let result = ZCString::with_source(source, |source| {
    ///     // make it clear we are working with an &str
    ///     // borrowed from source
    ///     let s: &str = &source;
    ///     s
    ///         .split(' ')
    ///         // ZCString::from(v: &str) checks does &str live in source?
    ///         .map(|v| ZCString::from(v))
    ///         // do we really point back to source?
    ///         .for_each(|v| assert!(source.source_of(&v)));
    /// });
    /// ```
    pub fn with_source<F, R>(source: ZCString, f: F) -> R
    where
        F: FnOnce(ZCString) -> R,
    {
        let guard = source.get_source_guard();
        let result = f(source);
        drop(guard);
        result
    }

    /// Transforms the current [`ZCString`] into a new view using a closure,
    /// provided the result is a sub-slice of the original.
    ///
    /// This is a high-level utility for performing zero-copy operations like
    /// trimming or pattern-based slicing using standard [`str`] methods.
    ///
    /// # Example
    /// ```
    /// use zcstring::ZCString;
    ///
    /// let zc = ZCString::from("  zero-copy  ");
    ///
    /// // Use map to trim the string without new allocations
    /// let trimmed = zc.map(|s| s.trim());
    ///
    /// assert_eq!(trimmed, "zero-copy");
    /// ```
    pub fn map<F>(&self, f: F) -> ZCString
    where
        F: FnOnce(&str) -> &str,
    {
        self.from_substr(f(self))
    }

    /// Wraps a standard string iterator to produce [`ZCString`] items instead of `&str`.
    ///
    /// This method allows you to leverage existing [`str`] iteration logic (like `.lines()` or `.split()`)
    /// while automatically promoting each yielded slice into a zero-copy [`ZCString`].
    ///
    /// The resulting items share the same underlying [`arcstr::ArcStr`] as this source,
    /// ensuring memory stays alive as long as any yielded item exists.
    ///
    /// # Arguments
    /// * `f` - A closure that takes a reference to the inner string and returns an iterator yielding `&str`.
    ///
    /// # Example
    /// ```
    /// use zcstring::ZCString;
    ///
    /// let zc = ZCString::from("line1\nline2\nline3");
    ///
    /// // Wrap the standard .lines() iterator
    /// let mut iter = zc.wrap_iter(|s| s.lines());
    ///
    /// assert_eq!(iter.next().unwrap(), "line1");
    /// assert_eq!(iter.next().unwrap(), "line2");
    /// ```
    pub fn wrap_iter<'a, F, I>(&'a self, f: F) -> ZCStringIterWrapper<'a, I>
    where
        F: FnOnce(&'a str) -> I,
        I: Iterator<Item = &'a str>,
    {
        ZCStringIterWrapper {
            source: self.clone(),
            inner: f(self.as_str()),
            _marker: std::marker::PhantomData,
        }
    }

    /// Create a ZCString by reading a range of bytes from
    /// an object supporting Read and Seek traits. The range must
    /// contain valid UTF-8
    ///
    /// # Arguments
    /// * `input` - A mutable reference to an object implementing [`Read`] and [`Seek`].
    /// * `range` - The byte range to read from the stream.
    ///
    /// # Errors
    /// * [`ReaderError::InvalidRange`] - If `start > end`, `end` exceeds stream length,
    ///   or bounds overflow `usize`.
    /// * [`ReaderError::Io`] - If seeking or reading from the stream fails.
    /// * [`ReaderError::Utf8`] - If the read bytes are not valid UTF-8.
    ///
    /// # Example
    /// ```
    /// use std::io::Cursor;
    /// use zcstring::ZCString;
    ///
    /// fn main() -> Result<(), Box<dyn std::error::Error>> {
    ///     // test data in a form that supports Read & Seek traits
    ///     // as if coming from a File
    ///     let mut data = Cursor::new(b"Cats and dogs");
    ///     // read "and" from 'data'
    ///     let mut r = ZCString::read_range(&mut data, 5..8)?;
    ///     assert_eq!(r, "and");
    ///     Ok(())
    /// }
    /// ```
    pub fn read_range<I, R>(input: &mut I, range: R) -> Result<ZCString, ReaderError>
    where
        I: Read + Seek,
        R: RangeBounds<u64>,
    {
        let start_pos = match range.start_bound() {
            Bound::Included(&s) => Some(s),
            Bound::Excluded(&s) => s.checked_add(1),
            Bound::Unbounded => Some(0),
        };

        let end_pos = match range.end_bound() {
            Bound::Included(&e) => e.checked_add(1),
            Bound::Excluded(&e) => Some(e),
            Bound::Unbounded => Some(input.seek(SeekFrom::End(0))?),
        };

        let (Some(start_pos), Some(end_pos)) = (start_pos, end_pos) else {
            return Err(ReaderError::InvalidRange {
                start: start_pos.unwrap_or(u64::MAX),
                end: end_pos.unwrap_or(u64::MAX),
            });
        };

        if start_pos > end_pos {
            return Err(ReaderError::InvalidRange {
                start: start_pos,
                end: end_pos,
            });
        }

        let stream_len = match range.end_bound() {
            Bound::Unbounded => end_pos,
            _ => input.seek(SeekFrom::End(0))?,
        };

        if end_pos > stream_len {
            return Err(ReaderError::InvalidRange {
                start: start_pos,
                end: end_pos,
            });
        }

        let len = end_pos - start_pos;
        if len == 0 {
            return Ok(ZCString::new());
        }

        let len = usize::try_from(len).map_err(|_| ReaderError::InvalidRange {
            start: start_pos,
            end: end_pos,
        })?;

        let mut io_error = Ok(());

        let result = ArcStr::init_with(len, |buffer| {
            io_error = (|| -> Result<(), std::io::Error> {
                input.seek(SeekFrom::Start(start_pos))?;
                input.read_exact(buffer)?;
                Ok(())
            })()
        });

        // deal with one of two error sources, I/O errors take priority
        match (io_error, result) {
            (Ok(()), Ok(result)) => Ok(ZCString::from(result)),
            (Err(e), _) => Err(e)?,
            (_, Err(e)) => Err(e)?,
        }
    }

    /// Create a ZCString by reading bytes from an object supporting the Read trait.
    /// The bytes must be valid UTF-8
    ///
    /// # Arguments
    /// * `input` - A mutable reference to an object implementing [`Read`].
    /// * `bytes` - The number of bytes to read. Must be greater than 0.
    ///
    /// # Panics
    /// Panics if `bytes == 0` because [`ArcStr::init_with`] requires non-zero size.
    ///
    /// # Errors
    /// * [`ReaderError::Io`] - If fewer than `bytes` can be read or an I/O error occurs.
    /// * [`ReaderError::Utf8`] - If the read bytes are not valid UTF-8.
    ///
    /// # Example
    /// ```
    /// use std::io::Cursor;
    /// use zcstring::ZCString;
    ///
    /// fn main() -> Result<(), Box<dyn std::error::Error>> {
    ///     // test data in a form that supports Read & Seek traits
    ///     // as if coming from a File
    ///     let mut data = Cursor::new(b"Cats and dogs");
    ///     // read "Cats" from 'data'
    ///     let mut r = ZCString::read_exact(&mut data, 4)?;
    ///     assert_eq!(r, "Cats");
    ///     Ok(())
    /// }
    /// ```
    pub fn read_exact<I: Read>(input: &mut I, bytes: usize) -> Result<ZCString, ReaderError> {
        let mut io_error = Ok(());

        let result = ArcStr::init_with(bytes, |buffer| {
            io_error = (|| -> Result<(), std::io::Error> {
                input.read_exact(buffer)?;
                Ok(())
            })()
        });

        // deal with one of two error sources, I/O errors take priority
        match (io_error, result) {
            (Ok(()), Ok(result)) => Ok(ZCString::from(result)),
            (Err(e), _) => Err(e)?,
            (_, Err(e)) => Err(e)?,
        }
    }

    /// Create a ZCString by reading an entire file
    ///
    /// # Arguments
    /// * `path` - Path to the file to open and read.
    ///
    /// # Errors
    /// * [`ReaderError::Io`] - If opening or reading the file fails.
    /// * [`ReaderError::Utf8`] - If file contents are not valid UTF-8.
    /// * [`ReaderError::InvalidRange`] - If the file size exceeds addressable integer limits.
    ///
    /// # Example
    /// ```
    /// use zcstring::ZCString;
    /// use std::path::PathBuf;
    ///
    /// fn main() -> Result<(), Box<dyn std::error::Error>> {
    ///     // Construct path relative to the project root
    ///     let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    ///     path.push("examples");
    ///     path.push("from_file_test.txt");
    ///     let r = ZCString::from_file(path)?;
    ///     assert_eq!(&r, "xyzzy");
    ///     Ok(())
    /// }
    /// ```
    pub fn from_file<P: AsRef<std::path::Path>>(path: P) -> Result<ZCString, ReaderError> {
        let mut handle = std::fs::File::open(path)?;
        Self::read_range(&mut handle, 0..)
    }
}

impl Default for ZCString {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq<str> for ZCString {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for ZCString {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<ZCString> for str {
    fn eq(&self, other: &ZCString) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<ZCString> for &str {
    fn eq(&self, other: &ZCString) -> bool {
        self == &**other
    }
}

impl PartialEq<String> for ZCString {
    fn eq(&self, other: &String) -> bool {
        self.0 == *other
    }
}

impl PartialEq<ZCString> for String {
    fn eq(&self, other: &ZCString) -> bool {
        *self == other.0
    }
}

impl Deref for ZCString {
    type Target = Substr;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<str> for ZCString {
    fn as_ref(&self) -> &str {
        self
    }
}

impl std::borrow::Borrow<str> for ZCString {
    fn borrow(&self) -> &str {
        self
    }
}

impl std::fmt::Display for ZCString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::fmt::Debug for ZCString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

/// From<&str> will check for existence of &str within the current source
//             ZCString
impl From<&str> for ZCString {
    #[inline]
    fn from(s: &str) -> Self {
        ZCString::from_str_with_source(s)
    }
}

impl From<ArcStr> for ZCString {
    #[inline]
    fn from(s: ArcStr) -> Self {
        ZCString(Substr::from(s))
    }
}

impl From<Substr> for ZCString {
    #[inline]
    fn from(s: Substr) -> Self {
        ZCString(s)
    }
}

impl From<String> for ZCString {
    #[inline]
    fn from(s: String) -> Self {
        ZCString::from_str_without_source(&s)
    }
}

/// An RAII guard used to manage the lifecycle of the thread-local string source.
///
/// Created via [`ZCString::get_source_guard`]. When dropped, it automatically restores
/// the previous source string that was active before this guard was instantiated.
///
/// Note: This type contains a `PhantomData<*const ()>` marker, making it `!Send` and `!Sync`.
/// This prevents guards from being transferred across threads, ensuring thread-local source
/// stack integrity.
#[must_use = "the source guard will restore the previous source immediately if not bound to a variable"]
pub struct SourceGuard {
    old_source: Option<ZCString>,
    // make sure this can't be moved between threads
    _marker: std::marker::PhantomData<*const ()>,
}

impl Drop for SourceGuard {
    fn drop(&mut self) {
        SOURCE.with(|ctx| {
            let mut borrow = ctx.borrow_mut();
            std::mem::swap(&mut *borrow, &mut self.old_source);
        });
    }
}

impl<'de> Deserialize<'de> for ZCString {
    /// Custom deserializer that attempts to borrow from the thread-local source
    /// when encountering a string.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ZCStringVisitor;

        impl<'de> serde::de::Visitor<'de> for ZCStringVisitor {
            type Value = ZCString;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string that can be borrowed or owned")
            }

            // borrow will build an arcstr::Substr of the original JSON
            fn visit_borrowed_str<E>(self, s: &'de str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(ZCString::from_str_with_source(s))
            }

            // build an arcstr::Substr based on the full ArcStr of our
            // decoded string
            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(ZCString::from_str_with_source(s))
            }

            // build a ZCString copying from String s
            fn visit_string<E>(self, s: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(ZCString::from_str_without_source(s.as_str()))
            }
        }

        // deserialize using our visitor
        deserializer.deserialize_str(ZCStringVisitor)
    }
}

/// Parses a JSON string into type `T` while using the provided [`ZCString`] as
/// the active context for zero-copy deserialization.
///
/// Any [`ZCString`], [`ZCRawValue`], or [`ZCLazyValue`] fields within `T` that
/// contain unescaped strings will borrow directly from `json` without allocating.
/// Escaped strings (which must be unescaped into new memory) automatically fall
/// back to standard allocations.
///
/// # Arguments
/// * `json` - The JSON payload wrapped in a `ZCString` to serve as the active source.
///
/// # Returns
/// The deserialized value `T`.
///
/// # Errors
/// Returns [`serde_json::Error`] if JSON parsing fails or the input does not match `T`.
///
/// # Example
/// ```
/// use serde::Deserialize;
/// use zcstring::{serde_json_from_zcstring, ZCString};
///
/// #[derive(Deserialize, Debug, PartialEq)]
/// struct User {
///     name: ZCString,
///     id: u64,
/// }
///
/// let json = ZCString::from(r#"{"name":"Alice","id":123}"#);
/// let user: User = serde_json_from_zcstring(json.clone()).unwrap();
/// assert_eq!(user.name, "Alice");
/// assert_eq!(user.id, 123);
/// assert!(json.source_of(&user.name));
/// ```
pub fn serde_json_from_zcstring<T>(json: ZCString) -> Result<T, serde_json::Error>
where
    T: for<'de> Deserialize<'de>,
{
    ZCString::with_source(json, |j| serde_json::from_str::<T>(&j))
}

/// An iterator wrapper that converts yielded `&str` slices into zero-copy [`ZCString`] items.
///
/// Created via [`ZCString::wrap_iter`]. Each yielded item shares the underlying buffer of
/// the parent `ZCString`.
///
/// # Example
/// ```
/// use zcstring::ZCString;
///
/// let text = ZCString::from("apple\nbanana\ncherry");
/// let lines: Vec<ZCString> = text.wrap_iter(|s| s.lines()).collect();
/// assert_eq!(lines.len(), 3);
/// assert_eq!(lines[0], "apple");
/// assert!(text.source_of(&lines[0]));
/// ```
pub struct ZCStringIterWrapper<'a, I> {
    source: ZCString,
    inner: I,
    _marker: std::marker::PhantomData<&'a str>,
}

impl<'a, I> Iterator for ZCStringIterWrapper<'a, I>
where
    I: Iterator<Item = &'a str>,
{
    type Item = ZCString;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .next()
            .map(|slice| self.source.from_substr(slice))
    }
}

/// A Serde wrapper holding an unparsed raw JSON string slice within a [`ZCString`].
///
/// `ZCRawValue` behaves similarly to [`serde_json::value::RawValue`], but stores the raw JSON
/// as a reference-counted, zero-allocation [`ZCString`]. This allows preserving unparsed
/// JSON fragments with zero allocations when deserialized via [`serde_json_from_zcstring`].
///
/// # Example
/// ```
/// use zcstring::{ZCRawValue, ZCString, serde_json_from_zcstring};
/// use arcstr::literal_substr;
///
/// #[derive(serde::Deserialize, serde::Serialize)]
/// struct Envelope {
///     status: ZCString,
///     payload: ZCRawValue,
/// }
///
/// let json = ZCString::l(literal_substr!(r#"{"status":"ok","payload":{"user_id":42,"roles":["admin"]}}"#));
/// let env: Envelope = serde_json_from_zcstring(json.clone()).unwrap();
/// assert_eq!(env.status, "ok");
/// assert_eq!(env.payload.value(), r#"{"user_id":42,"roles":["admin"]}"#);
/// assert!(json.source_of(env.payload.value()));
/// ```
#[derive(Default, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ZCRawValue(ZCString);

impl ZCRawValue {
    /// Constructs a new [`ZCRawValue`] wrapping the provided raw JSON [`ZCString`].
    ///
    /// # Arguments
    /// * `v` - A `ZCString` containing raw JSON.
    ///
    /// # Example
    /// ```
    /// use zcstring::{ZCRawValue, ZCString};
    ///
    /// let raw = ZCRawValue::new(ZCString::from("{\"key\":\"value\"}"));
    /// assert_eq!(raw.value(), "{\"key\":\"value\"}");
    /// ```
    pub fn new(v: ZCString) -> Self {
        ZCRawValue(v)
    }

    /// Returns a reference to the inner raw JSON [`ZCString`].
    ///
    /// # Returns
    /// A reference to the wrapped `ZCString`.
    ///
    /// # Example
    /// ```
    /// use zcstring::{ZCRawValue, ZCString};
    ///
    /// let raw = ZCRawValue::new(ZCString::from("[1,2,3]"));
    /// assert_eq!(raw.value(), "[1,2,3]");
    /// ```
    pub fn value(&self) -> &ZCString {
        &self.0
    }

    /// Consumes the wrapper and returns the inner [`ZCString`].
    ///
    /// # Returns
    /// The owned `ZCString`.
    ///
    /// # Example
    /// ```
    /// use zcstring::{ZCRawValue, ZCString};
    ///
    /// let raw = ZCRawValue::new(ZCString::from("true"));
    /// let inner: ZCString = raw.into_value();
    /// assert_eq!(inner, "true");
    /// ```
    pub fn into_value(self) -> ZCString {
        self.0
    }
}

impl std::fmt::Debug for ZCRawValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0.as_str(), f)
    }
}

impl<'de> Deserialize<'de> for ZCRawValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw_val: &'de RawValue =
            Deserialize::deserialize(deserializer)?;

        Ok(ZCRawValue(ZCString::from_str_with_source(raw_val.get())))
    }
}

impl Serialize for ZCRawValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {    
        let raw = serde_json::from_str::<&serde_json::value::RawValue>(self.0.as_str())
            .map_err(S::Error::custom)?;
        raw.serialize(serializer)
    }
}

/// A Serde composite holding both the unparsed raw JSON string slice and an eagerly decoded value `T`.
///
/// This type is ideal when you need to inspect or operate on the strongly typed value `T`,
/// while simultaneously retaining the exact original raw JSON string (e.g. for cryptographic
/// hashing, caching, provenance, or forwarding to downstream systems).
///
/// Note: Comparison traits ([`PartialEq`], [`Eq`], [`PartialOrd`], [`Ord`]) and [`Hash`]
/// operate exclusively on the `decoded` value `T`, ignoring whitespace or formatting
/// differences in `raw`.
///
/// # Example
/// ```
/// use zcstring::{ZCRawValueAnd, ZCString, serde_json_from_zcstring};
/// use arcstr::literal_substr;
///
/// #[derive(serde::Deserialize)]
/// struct Response {
///     status: ZCString,
///     items: ZCRawValueAnd<Vec<u32>>,
/// }
///
/// let json = ZCString::l(literal_substr!(r#"{"status":"success","items":[1, 2,  3]}"#));
/// let resp: Response = serde_json_from_zcstring(json)?;
/// assert_eq!(resp.items.raw(), "[1, 2,  3]");
/// assert_eq!(resp.items.value().as_slice(), &[1, 2, 3]);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct ZCRawValueAnd<T> {
    raw: ZCRawValue,
    decoded: T,
}

impl<T: DeserializeOwned> ZCRawValueAnd<T> {
    /// Constructs a `ZCRawValueAnd` by parsing `raw` into `T` while preserving the raw JSON slice.
    ///
    /// Uses [`serde_json_from_zcstring`] so that any `ZCString` members in `T` borrow
    /// zero-copy from `raw`.
    ///
    /// # Arguments
    /// * `raw` - A `ZCString` containing the raw JSON to parse.
    ///
    /// # Errors
    /// Returns [`serde_json::Error`] if deserializing `T` from `raw` fails.
    ///
    /// # Example
    /// ```
    /// use zcstring::{ZCRawValueAnd, ZCString};
    ///
    /// let raw = ZCString::from("[10, 20, 30]");
    /// let val = ZCRawValueAnd::<Vec<u32>>::new(raw).unwrap();
    /// assert_eq!(val.raw(), "[10, 20, 30]");
    /// assert_eq!(val.value().as_slice(), &[10, 20, 30]);
    /// ```
    pub fn new(raw: ZCString) -> Result<Self, serde_json::Error> {
        Ok(Self {
            decoded: serde_json_from_zcstring(raw.clone())?,
            raw: ZCRawValue(raw),
        })
    }

    /// Returns a reference to the unparsed raw JSON [`ZCString`].
    ///
    /// # Returns
    /// The raw JSON slice.
    pub fn raw(&self) -> &ZCString {
        self.raw.value()
    }

    /// Returns a reference to the decoded Rust value `T`.
    ///
    /// # Returns
    /// A reference to `decoded`.
    pub fn value(&self) -> &T {
        &self.decoded
    }
 
    /// Consumes the wrapper, returning the unparsed raw [`ZCString`] and the decoded `T`.
    ///
    /// # Returns
    /// A tuple of `(raw_zcstring, decoded_value)`.
    ///
    /// # Example
    /// ```
    /// use zcstring::{ZCRawValueAnd, ZCString};
    ///
    /// let val = ZCRawValueAnd::<u32>::new(ZCString::from("42")).unwrap();
    /// let (raw, num) = val.into_parts();
    /// assert_eq!(raw, "42");
    /// assert_eq!(num, 42);
    /// ```
    pub fn into_parts(self) -> (ZCString, T) {
        (self.raw.into_value(), self.decoded)
    }
}

impl<T> Deref for ZCRawValueAnd<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.decoded
    }
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for ZCRawValueAnd<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw: ZCRawValue = 
            Deserialize::deserialize(deserializer)?;
            
        // now deserialize the data component from our raw string
        // raw is a substring of our data source, we don't need
        // to re-specify it.
        let decoded =
            serde_json_from_zcstring(raw.value().clone())
            .map_err(serde::de::Error::custom)?;

        Ok(ZCRawValueAnd {
            raw,
            decoded,
        })
    }
}

impl<T> Serialize for ZCRawValueAnd<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // just send back the raw version
        self.raw.serialize(serializer)
    }
}

impl<T: fmt::Debug> fmt::Debug for ZCRawValueAnd<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZCRawValueAnd")
            .field("raw", &self.raw)
            .field("decoded", &self.decoded)
            .finish()
    }
}

impl<T: fmt::Display> fmt::Display for ZCRawValueAnd<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.decoded, f)
    }
}

impl<T: Clone> Clone for ZCRawValueAnd<T> {
    fn clone(&self) -> Self {
        Self {
            raw: self.raw.clone(),
            decoded: self.decoded.clone(),
        }
    }
}

impl<T: PartialEq> PartialEq for ZCRawValueAnd<T> {
    fn eq(&self, other: &Self) -> bool {
        self.decoded.eq(&other.decoded)
    }
}

impl<T: Eq> Eq for ZCRawValueAnd<T> {}

impl<T: PartialOrd> PartialOrd for ZCRawValueAnd<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.decoded.partial_cmp(&other.decoded)
    }
}

impl<T: Ord> Ord for ZCRawValueAnd<T> {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        self.decoded.cmp(&other.decoded)
    }
}

impl<T: Hash> Hash for ZCRawValueAnd<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.decoded.hash(state);
    }
}

impl<T: Default> Default for ZCRawValueAnd<T> {
    fn default() -> Self {
        Self {
            raw: ZCRawValue::default(),
            decoded: T::default(),
        }
    }
}

/// A Serde type that preserves unparsed raw JSON and defers deserializing into `T` until accessed.
///
/// `ZCLazyValue` is designed for high-performance systems processing large or complex JSON documents
/// where only a subset of fields are needed, or where expensive inner structures should only be
/// deserialized on demand.
///
/// # Generics and Const Parameters
///
/// - `T`: The Rust type to deserialize lazily. Must implement [`DeserializeOwned`].
/// - `const L: bool`: Error logging flag (defaults to `false`). When `true`, deserialization errors
///   are logged to [`log::error!`] upon failure. When `false`, errors are cached quietly without logging.
///
/// # Deferred Evaluation and Thread Safety
///
/// Parsing of the raw JSON slice is deferred until the first invocation of [`value`](Self::value)
/// (or trait methods like [`cmp`](Ord::cmp), [`eq`](PartialEq::eq), or [`hash`](Hash::hash)).
/// The result is computed once and stored in a thread-safe [`OnceLock`], wrapped in an [`Arc`].
/// Multiple threads can safely access `.value()` concurrently.
///
/// # Default Values and Serialization
///
/// When deserializing missing fields with `#[serde(default)]`, `ZCLazyValue` initializes as a
/// "value-only" instance wrapping `T::default()`. Using
/// `#[serde(default, skip_serializing_if = "ZCLazyValue::is_value_only")]` allows cleanly
/// omitting missing fields during re-serialization.
///
/// # Example
///
/// ```
/// use zcstring::{ZCLazyValue, ZCString, serde_json_from_zcstring};
/// use arcstr::literal_substr;
/// use std::sync::Arc;
///
/// #[derive(serde::Deserialize, serde::Serialize, Debug)]
/// struct Payload {
///     tag: ZCString,
///     #[serde(default, skip_serializing_if = "ZCLazyValue::is_value_only")]
///     stats: ZCLazyValue<Vec<u32>>,
/// }
///
/// let json = ZCString::l(literal_substr!(r#"{"tag":"sensor-1","stats":[10,20,30]}"#));
/// let p: Payload = serde_json_from_zcstring(json)?;
/// assert_eq!(p.stats.raw().map(|s| s.as_str()), Some("[10,20,30]"));
/// assert!(!p.stats.is_decoded());
///
/// // First access triggers deserialization:
/// let numbers = p.stats.value().unwrap();
/// assert_eq!(numbers.as_slice(), &[10, 20, 30]);
/// assert!(p.stats.is_decoded());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone)]
pub struct ZCLazyValue<T, const L: bool = false> {
    raw: Option<ZCRawValue>,
    decoded: OnceLock<Result<Arc<T>, Arc<serde_json::Error>>>,
}

impl<T: for<'de> Deserialize<'de>, const L: bool> ZCLazyValue<T, L> {
    /// Constructs a `ZCLazyValue` from a raw JSON [`ZCString`].
    ///
    /// Parsing into `T` is deferred until [`value`](Self::value) or a comparison/hash method
    /// is first called.
    ///
    /// # Arguments
    /// * `raw` - A `ZCString` containing the raw JSON to be lazily parsed.
    ///
    /// # Example
    /// ```
    /// use zcstring::{ZCLazyValue, ZCString};
    ///
    /// let lazy = ZCLazyValue::<u32>::from_raw(ZCString::from("123"));
    /// assert!(!lazy.is_decoded());
    /// assert_eq!(**lazy.value().unwrap(), 123);
    /// assert!(lazy.is_decoded());
    /// ```
    pub fn from_raw(raw: ZCString) -> Self {
        Self {
            raw: Some(ZCRawValue::new(raw)),
            decoded: OnceLock::new(),
        }
    }

    /// Constructs a `ZCLazyValue` directly from an existing value `Arc<T>`.
    ///
    /// The resulting instance has no raw JSON (`raw() == None`) and is considered
    /// "value-only" ([`is_value_only`](Self::is_value_only)).
    ///
    /// # Arguments
    /// * `t` - An `Arc` containing the pre-constructed value.
    ///
    /// # Example
    /// ```
    /// use std::sync::Arc;
    /// use zcstring::ZCLazyValue;
    ///
    /// let val = ZCLazyValue::<String>::from_value(Arc::new(String::from("ready")));
    /// assert!(val.is_value_only());
    /// assert_eq!(val.value().unwrap().as_str(), "ready");
    /// ```
    pub fn from_value(t: Arc<T>) -> Self {
        Self {
            raw: None,
            decoded: OnceLock::from(Ok(t)),
        }
    }

    /// Constructs a `ZCLazyValue` directly from a pre-existing deserialization error.
    ///
    /// # Arguments
    /// * `e` - An `Arc` containing the deserialization error.
    ///
    /// # Example
    /// ```
    /// use std::sync::Arc;
    /// use zcstring::ZCLazyValue;
    ///
    /// let err = serde_json::from_str::<u32>("invalid").unwrap_err();
    /// let lazy = ZCLazyValue::<u32>::from_error(Arc::new(err));
    /// assert!(lazy.value().is_err());
    /// ```
    pub fn from_error(e: Arc<serde_json::Error>) -> Self {
        Self {
            raw: None,
            decoded: OnceLock::from(Err(e)),
        }
    }

    /// Returns a reference to the unparsed raw JSON [`ZCString`], if present.
    ///
    /// Returns `None` if this instance was constructed without a raw string
    /// (e.g. via [`from_value`](Self::from_value) or [`Default::default`]).
    ///
    /// # Returns
    /// An `Option` referencing the raw JSON `ZCString`.
    pub fn raw(&self) -> Option<&ZCString> {
        self.raw.as_ref().map(|v| v.value())
    }

    // internal call to OnceLock
    fn get_or_init(&self) -> &Result<Arc<T>, Arc<serde_json::Error>> {
        self.decoded
            .get_or_init(|| {
                match &self.raw {
                    Some(raw) =>
                        match serde_json_from_zcstring(raw.value().clone()) {
                            Ok(t) => Ok(Arc::new(t)),
                            Err(e) => {
                                if L {
                                    error!("ZCLazyValue parse failure: {:?} from input {:?}", e, raw.value());
                                }
                                Err(Arc::new(e))
                            }
                        }

                    // we shouldn't get here, but better to handle it sensibly
                    None => Err(Arc::new(
                        serde::de::Error::custom("ZCLazyValue raw string is None"))),
                }
            })
    }

    /// Returns a reference to the decoded value `T`, or an error if deserialization failed.
    ///
    /// The first call to this method triggers parsing from the raw JSON string and caches
    /// the result in an internal [`OnceLock`]. Subsequent calls return the cached reference
    /// with zero parsing overhead.
    ///
    /// # Returns
    /// - `Ok(&Arc<T>)` if deserialization succeeded.
    /// - `Err(&Arc<serde_json::Error>)` if deserialization failed.
    pub fn value(&self) -> Result<&Arc<T>, &Arc<serde_json::Error>> {
        self.get_or_init().as_ref()
    }

    /// Returns `true` if the value has already been evaluated or decoded.
    ///
    /// # Returns
    /// `true` if the inner [`OnceLock`] has been initialized.
    pub fn is_decoded(&self) -> bool {
        self.decoded.get().is_some()
    }

    /// Returns `true` if this instance contains only a Rust value with no raw JSON slice.
    ///
    /// This returns `true` for instances created via [`from_value`](Self::from_value) or
    /// [`Default::default`]. Commonly used with `skip_serializing_if = "ZCLazyValue::is_value_only"`.
    ///
    /// # Returns
    /// `true` if `raw` is `None`.
    pub fn is_value_only(&self) -> bool {
        self.raw.is_none()
    }    
}

impl<'de, T, const L: bool> Deserialize<'de> for ZCLazyValue<T, L> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw: Option<ZCRawValue> = 
            Some(Deserialize::deserialize(deserializer)?);

        // now deserialize the data component from our raw string
        // raw is a substring of our data source, we don't need
        // to re-specify it.
        Ok(ZCLazyValue {
            raw,
            decoded: OnceLock::new(),
        })
    }
}

impl<T: for<'de> Deserialize<'de> + Serialize, const L: bool> Serialize for ZCLazyValue<T, L> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // just send back the raw version if we came from
        if let Some(raw) = &self.raw {
            raw.serialize(serializer)

        } else if let Ok(decoded) = self.value() {
            decoded.serialize(serializer)

        } else {
            Err(S::Error::custom("Deserialize error can't be encoded"))
        }
    }
}

impl<T: Debug, const L: bool> fmt::Debug for ZCLazyValue<T, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZCLazyValue")
            .field("raw", &self.raw)
            .field("decoded", &self.decoded)
            .finish()
    }
}

impl<T: for<'de> Deserialize<'de> + PartialEq, const L: bool> PartialEq for ZCLazyValue<T, L> {
    fn eq(&self, other: &Self) -> bool {
        match (self.value(), other.value()) {
            // compare decoded contents
            (Ok(a), Ok(b)) => a.eq(b),
            // error vs. non error is false
            (Ok(_), _) => false,
            (_, Ok(_)) => false,
            // treat all error values as equal
            _ => true,
        }
    }
}

impl<T: for<'de> Deserialize<'de> + Eq, const L: bool> Eq for ZCLazyValue<T, L> {}

impl<T: for<'de> Deserialize<'de> + PartialOrd, const L: bool> PartialOrd for ZCLazyValue<T, L> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self.value(), other.value()) {
            // compare decoded contents
            (Ok(a), Ok(b)) => a.partial_cmp(b),
            // value > error
            (Ok(_), _) => Some(Ordering::Greater),
            (_, Ok(_)) => Some(Ordering::Less),
            // treat all error values as equal
            _ => Some(Ordering::Equal),
        }
    }
}

impl<T: for<'de> Deserialize<'de> + Ord, const L: bool> Ord for ZCLazyValue<T, L> {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.value(), other.value()) {
            // compare decoded contents
            (Ok(a), Ok(b)) => a.cmp(b),
            // value > error
            (Ok(_), _) => Ordering::Greater,
            (_, Ok(_)) => Ordering::Less,
            // treat all error values as equal
            _ => Ordering::Equal,
        }
    }
}

impl<T: for<'de> Deserialize<'de> + Hash, const L: bool> Hash for ZCLazyValue<T, L> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self.value() {
            Ok(v) => {
                1_u8.hash(state);
                v.hash(state)
            },

            _ => {
                0_u8.hash(state)
            }
        }
    }
}

impl<T: Default, const L: bool> Default for ZCLazyValue<T, L> {
    fn default() -> Self {
        Self {
            raw: None,
            decoded: OnceLock::from(Ok(Arc::new(T::default()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::io::Cursor;
    use std::thread;

    #[test]
    fn test_zcstring_new_and_empty() {
        let s = ZCString::new();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert_eq!(s.as_str(), "");
        assert_eq!(s, "");
        assert_eq!(s, ZCString::default());
    }

    #[test]
    fn test_zcstring_literals() {
        const STATIC_STR: ZCString = ZCString::l(literal_substr!("compile_time_literal"));
        assert_eq!(STATIC_STR, "compile_time_literal");
        assert_eq!(STATIC_STR.len(), 20);
        assert!(!STATIC_STR.is_empty());
    }

    #[test]
    fn test_zcstring_from_conversions() {
        let from_str: ZCString = ZCString::from("from_str");
        assert_eq!(from_str, "from_str");

        let from_string: ZCString = ZCString::from(String::from("from_string"));
        assert_eq!(from_string, "from_string");

        let arc_str = ArcStr::from("from_arcstr");
        let from_arcstr: ZCString = ZCString::from(arc_str.clone());
        assert_eq!(from_arcstr, "from_arcstr");

        let substr = Substr::from(arc_str);
        let from_substr_type: ZCString = ZCString::from(substr);
        assert_eq!(from_substr_type, "from_arcstr");

        let without_source = ZCString::from_str_without_source("without_source");
        assert_eq!(without_source, "without_source");
    }

    #[test]
    fn test_zcstring_source_of_and_substr() {
        let root = ZCString::from_str_without_source("The quick brown fox jumps over the lazy dog");

        // Substr checks
        let quick = root.substr(4..9);
        assert_eq!(quick, "quick");
        assert!(root.source_of(&quick));

        let fox = root.substr(16..19);
        assert_eq!(fox, "fox");
        assert!(root.source_of(&fox));

        // Prefix, suffix, and whole string
        assert!(root.source_of(&root[0..3]));
        assert!(root.source_of(&root[35..]));
        assert!(root.source_of(&root[..]));

        // Empty slices at various offsets
        assert!(root.source_of(&root[0..0]));
        assert!(root.source_of(&root[10..10]));
        assert!(root.source_of(&root[root.len()..root.len()]));

        // Nested substring: grandchild
        let sub = root.substr(4..19); // "quick brown fox"
        let grandchild = sub.substr(6..11); // "brown"
        assert_eq!(grandchild, "brown");
        assert!(root.source_of(&grandchild));
        assert!(sub.source_of(&grandchild));

        // External strings should not be in source
        let stack_str = "quick";
        assert!(!root.source_of(stack_str));

        let heap_string = String::from("quick");
        assert!(!root.source_of(&heap_string));

        let other_root = ZCString::from_str_without_source("The quick brown fox jumps over the lazy dog");
        assert!(!root.source_of(&other_root));
        assert!(!root.source_of(&other_root[4..9]));

        // Edge case: Empty root
        let empty_root = ZCString::new();
        assert!(empty_root.source_of(&empty_root[0..0]));
    }

    #[test]
    fn test_zcstring_from_substr() {
        let root = ZCString::from_str_without_source("hello zero copy world");

        // Sub-slice residing within root
        let slice = &root[6..15]; // "zero copy"
        let zc_slice = root.from_substr(slice);
        assert_eq!(zc_slice, "zero copy");
        assert!(root.source_of(&zc_slice));

        // External slice not residing within root falls back to allocation
        let foreign = "external string";
        let zc_foreign = root.from_substr(foreign);
        assert_eq!(zc_foreign, "external string");
        assert!(!root.source_of(&zc_foreign));
    }

    #[test]
    fn test_zcstring_detach() {
        let large = ZCString::from_str_without_source("abcdefghijklmnopqrstuvwxyz");
        let sub = large.substr(10..15); // "klmno"
        assert_eq!(sub, "klmno");
        assert!(large.source_of(&sub));

        let detached = sub.detach();
        assert_eq!(detached, "klmno");
        assert_eq!(detached, sub);
        // The detached string buffer is a new allocation, so large is no longer its source
        assert!(!large.source_of(&detached));
    }

    #[test]
    fn test_source_guard_nested_and_lifecycles() {
        let s1 = ZCString::from_str_without_source("outer buffer source 1");
        let s2 = ZCString::from_str_without_source("inner buffer source 2");

        {
            let guard1 = s1.get_source_guard();
            let slice1 = &s1[0..5]; // "outer"
            let zc1 = ZCString::from_str_with_source(slice1);
            assert_eq!(zc1, "outer");
            assert!(s1.source_of(&zc1));

            // Nested guard
            {
                let guard2 = s2.get_source_guard();
                let slice2 = &s2[0..5]; // "inner"
                let zc2 = ZCString::from_str_with_source(slice2);
                assert_eq!(zc2, "inner");
                assert!(s2.source_of(&zc2));
                // Slice from s1 is not in s2, so it falls back to allocation
                let zc1_fallback = ZCString::from_str_with_source(slice1);
                assert_eq!(zc1_fallback, "outer");
                assert!(!s2.source_of(&zc1_fallback));
                drop(guard2);
            }

            // guard2 dropped -> s1 restored as active source
            let zc1_restored = ZCString::from_str_with_source(slice1);
            assert_eq!(zc1_restored, "outer");
            assert!(s1.source_of(&zc1_restored));

            drop(guard1);
        }

        // guard1 dropped -> SOURCE restored to None
        let slice1 = &s1[0..5];
        let zc_none = ZCString::from_str_with_source(slice1);
        assert_eq!(zc_none, "outer");
        // Without active source, it allocates
        assert!(!s1.source_of(&zc_none));
    }

    #[test]
    fn test_with_source() {
        let source = ZCString::from("apple banana cherry date");

        let words = ZCString::with_source(source.clone(), |src| {
            let s: &str = &src;
            s.split(' ')
                .map(ZCString::from)
                .collect::<Vec<ZCString>>()
        });

        assert_eq!(words.len(), 4);
        assert_eq!(words[0], "apple");
        assert_eq!(words[1], "banana");
        assert_eq!(words[2], "cherry");
        assert_eq!(words[3], "date");

        for word in &words {
            assert!(source.source_of(word));
        }

        // After with_source returns, SOURCE should be restored to None
        let sample = "apple";
        let orphan = ZCString::from(sample);
        assert!(!source.source_of(&orphan));
    }

    #[test]
    fn test_thread_local_isolation() {
        let source_main = ZCString::from_str_without_source("main thread source");
        let _guard = source_main.get_source_guard();

        // Spawn a thread and check that SOURCE in the new thread is initially None
        let handle = thread::spawn(|| {
            let test_str = "worker string";
            let zc = ZCString::from_str_with_source(test_str);
            assert_eq!(zc, "worker string");

            // Setting source in child thread
            let child_source = ZCString::from_str_without_source("child thread source");
            let _child_guard = child_source.get_source_guard();
            let sub = ZCString::from_str_with_source(&child_source[0..5]);
            assert!(child_source.source_of(&sub));
        });

        handle.join().unwrap();

        // Main thread source remains intact
        let main_slice = &source_main[0..4];
        let main_zc = ZCString::from_str_with_source(main_slice);
        assert!(source_main.source_of(&main_zc));
    }

    #[test]
    fn test_zcstring_map() {
        let root = ZCString::from("  padded text  ");
        let trimmed = root.map(|s| s.trim());
        assert_eq!(trimmed, "padded text");
        assert!(root.source_of(&trimmed));

        // Mapping to a non-subslice falls back to allocation
        let foreign = root.map(|_| "unrelated static str");
        assert_eq!(foreign, "unrelated static str");
        assert!(!root.source_of(&foreign));
    }

    #[test]
    fn test_zcstring_wrap_iter() {
        let text = ZCString::from("alpha\nbeta\ngamma\ndelta");
        let lines: Vec<ZCString> = text.wrap_iter(|s| s.lines()).collect();

        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], "alpha");
        assert_eq!(lines[1], "beta");
        assert_eq!(lines[2], "gamma");
        assert_eq!(lines[3], "delta");

        for line in &lines {
            assert!(text.source_of(line));
        }

        // Empty wrap_iter
        let empty = ZCString::new();
        let mut empty_iter = empty.wrap_iter(|s| s.lines());
        assert!(empty_iter.next().is_none());
    }

    #[test]
    fn test_read_range() {
        let mut data = Cursor::new(b"0123456789abcdef");

        // Included .. Excluded
        let r1 = ZCString::read_range(&mut data, 0..5).unwrap();
        assert_eq!(r1, "01234");

        // Unbounded end
        let r2 = ZCString::read_range(&mut data, 10..).unwrap();
        assert_eq!(r2, "abcdef");

        // Unbounded start
        let r3 = ZCString::read_range(&mut data, ..4).unwrap();
        assert_eq!(r3, "0123");

        // Entire stream unbounded
        let r4 = ZCString::read_range(&mut data, ..).unwrap();
        assert_eq!(r4, "0123456789abcdef");

        // Empty range
        let r5 = ZCString::read_range(&mut data, 7..7).unwrap();
        assert!(r5.is_empty());
        assert_eq!(r5, "");

        // Error: start > end
        let err_inverted = ZCString::read_range(&mut data, 8..4);
        assert!(matches!(err_inverted, Err(ReaderError::InvalidRange { start: 8, end: 4 })));

        // Error: end > stream length
        let err_past_end = ZCString::read_range(&mut data, 0..100);
        assert!(matches!(err_past_end, Err(ReaderError::InvalidRange { .. })));

        // Error: Invalid UTF-8
        let mut invalid_utf8 = Cursor::new(b"\xFF\xFE\xFD");
        let err_utf8 = ZCString::read_range(&mut invalid_utf8, 0..3);
        assert!(matches!(err_utf8, Err(ReaderError::Utf8(_))));
    }

    #[test]
    fn test_read_exact() {
        let mut data = Cursor::new(b"Hello world");

        let r1 = ZCString::read_exact(&mut data, 5).unwrap();
        assert_eq!(r1, "Hello");

        let r2 = ZCString::read_exact(&mut data, 6).unwrap();
        assert_eq!(r2, " world");

        // Error: Read beyond end
        let mut short_data = Cursor::new(b"short");
        let err_eof = ZCString::read_exact(&mut short_data, 10);
        assert!(matches!(err_eof, Err(ReaderError::Io(_))));

        // Error: Invalid UTF-8
        let mut bad_utf8 = Cursor::new(vec![0xFF, 0xFF]);
        let err_utf8 = ZCString::read_exact(&mut bad_utf8, 2);
        assert!(matches!(err_utf8, Err(ReaderError::Utf8(_))));
    }

    #[test]
    #[should_panic]
    fn test_read_exact_zero_bytes_panics() {
        let mut data = Cursor::new(b"Hello world");
        let _ = ZCString::read_exact(&mut data, 0);
    }

    #[test]
    fn test_from_file() {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("examples");
        path.push("from_file_test.txt");

        let content = ZCString::from_file(&path).unwrap();
        assert_eq!(content, "xyzzy");

        // Error on non-existent file
        let bad_path = std::path::PathBuf::from("does_not_exist_xyz123.txt");
        let err_file = ZCString::from_file(&bad_path);
        assert!(matches!(err_file, Err(ReaderError::Io(_))));
    }

    #[test]
    fn test_reader_error_display() {
        let err_range = ReaderError::InvalidRange { start: 10, end: 5 };
        assert_eq!(format!("{}", err_range), "Invalid range: start 10, end 5");

        let err_io = ReaderError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "file missing"));
        assert!(format!("{}", err_io).contains("IO failure: file missing"));

        let bad_vec = vec![0xfe, 0xfe];
        let utf8_err = std::str::from_utf8(&bad_vec).unwrap_err();
        let err_utf8 = ReaderError::Utf8(utf8_err);
        assert!(format!("{}", err_utf8).contains("UTF-8 encoding failure:"));
    }

    #[test]
    fn test_zcstring_traits_and_comparisons() {
        let zc = ZCString::from("test_string");
        let other_zc = ZCString::from("test_string");
        let different_zc = ZCString::from("other_string");

        // Equality with self and other ZCString
        assert_eq!(zc, other_zc);
        assert_ne!(zc, different_zc);

        // Equality with &str and str
        assert_eq!(zc, "test_string");
        assert_eq!("test_string", zc);
        assert_eq!(*zc, "test_string");

        // Equality with String
        let s = String::from("test_string");
        assert_eq!(zc, s);
        assert_eq!(s, zc);

        // Ordering
        assert!(different_zc < zc);
        assert_eq!(zc.cmp(&other_zc), Ordering::Equal);
        assert_eq!(different_zc.cmp(&zc), Ordering::Less);

        // Deref
        assert_eq!(zc.len(), 11);
        assert!(!zc.is_empty());
        assert_eq!(&zc[0..4], "test");

        // AsRef and Borrow
        assert_eq!(zc.as_ref(), "test_string");
        let borrowed: &str = std::borrow::Borrow::borrow(&zc);
        assert_eq!(borrowed, "test_string");

        // Display and Debug
        assert_eq!(format!("{}", zc), "test_string");
        assert_eq!(format!("{:?}", zc), "\"test_string\"");
    }

    #[test]
    fn test_zcstring_hash_and_borrow_in_map() {
        let mut map: HashMap<ZCString, i32> = HashMap::new();
        let key = ZCString::from("lookup_key");
        map.insert(key.clone(), 42);

        // Querying HashMap using &str thanks to Borrow<str>
        assert_eq!(map.get("lookup_key"), Some(&42));
        assert_eq!(map.get(&key), Some(&42));
        assert_eq!(map.get("missing_key"), None);

        let mut set: HashSet<ZCString> = HashSet::new();
        set.insert(key);
        assert!(set.contains("lookup_key"));
    }

    #[test]
    fn test_serde_json_deserialization() {
        #[derive(Deserialize, Serialize, Debug, PartialEq)]
        struct Person {
            first_name: ZCString,
            last_name: ZCString,
            notes: ZCString,
        }

        // Unescaped JSON strings: should borrow zero-copy
        let json_input = ZCString::from(r#"{"first_name":"Grace","last_name":"Hopper","notes":"Compiler pioneer"}"#);
        let person: Person = serde_json_from_zcstring(json_input.clone()).unwrap();

        assert_eq!(person.first_name, "Grace");
        assert_eq!(person.last_name, "Hopper");
        assert_eq!(person.notes, "Compiler pioneer");

        assert!(json_input.source_of(&person.first_name));
        assert!(json_input.source_of(&person.last_name));
        assert!(json_input.source_of(&person.notes));

        // Escaped JSON string: serde unescapes it into a new String, so it falls back to allocation
        let json_escaped = ZCString::from(r#"{"first_name":"Grace","last_name":"O\"Connor","notes":"Line\nBreak"}"#);
        let person_escaped: Person = serde_json_from_zcstring(json_escaped.clone()).unwrap();

        assert_eq!(person_escaped.first_name, "Grace");
        assert_eq!(person_escaped.last_name, "O\"Connor");
        assert_eq!(person_escaped.notes, "Line\nBreak");

        assert!(json_escaped.source_of(&person_escaped.first_name));
        // Escaped fields do not reside in original buffer
        assert!(!json_escaped.source_of(&person_escaped.last_name));
        assert!(!json_escaped.source_of(&person_escaped.notes));

        // Syntax error test
        let malformed = ZCString::from(r#"{"first_name": "Broken"#);
        let err = serde_json_from_zcstring::<Person>(malformed);
        assert!(err.is_err());
    }

    #[test]
    fn test_serde_json_serialization() {
        let zc = ZCString::from("serialize_me");
        let serialized = serde_json::to_string(&zc).unwrap();
        assert_eq!(serialized, "\"serialize_me\"");
    }

    #[test]
    fn test_zc_raw_value() {
        #[derive(Deserialize, Serialize, Debug)]
        struct Message {
            id: u64,
            payload: ZCRawValue,
        }

        let json = ZCString::from(r#"{"id":100,"payload":{"key":"value","items":[1,2,3]}}"#);
        let msg: Message = serde_json_from_zcstring(json.clone()).unwrap();

        assert_eq!(msg.id, 100);
        assert_eq!(msg.payload.value(), r#"{"key":"value","items":[1,2,3]}"#);
        assert!(json.source_of(msg.payload.value()));

        // Serializing Message
        let reserialized = serde_json::to_string(&msg).unwrap();
        assert_eq!(reserialized, r#"{"id":100,"payload":{"key":"value","items":[1,2,3]}}"#);

        // Constructor and into_value
        let raw_zc = ZCString::from("{\"num\":42}");
        let raw_val = ZCRawValue::new(raw_zc.clone());
        assert_eq!(raw_val.value(), "{\"num\":42}");
        assert_eq!(raw_val.into_value(), raw_zc);

        // Debug, Default, Clone, Eq, Hash
        let default_raw = ZCRawValue::default();
        assert_eq!(default_raw.value(), "");

        let cloned_raw = default_raw.clone();
        assert_eq!(default_raw, cloned_raw);
        assert_eq!(format!("{:?}", default_raw), "\"\"");

        let mut set = HashSet::new();
        set.insert(default_raw);
        assert!(set.contains(&ZCRawValue::default()));

        // Invalid JSON serialization should fail
        let invalid_raw = ZCRawValue::new(ZCString::from("invalid json {{{"));
        let serialize_err = serde_json::to_string(&invalid_raw);
        assert!(serialize_err.is_err());
    }

    #[test]
    fn test_zc_raw_value_and() {
        #[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        struct Inner {
            count: u32,
            tag: ZCString,
        }

        #[derive(Deserialize, Serialize, Debug)]
        struct Envelope {
            data: ZCRawValueAnd<Inner>,
        }

        let json = ZCString::from(r#"{"data":{"count":7,"tag":"special"}}"#);
        let env: Envelope = serde_json_from_zcstring(json.clone()).unwrap();

        assert_eq!(env.data.raw(), r#"{"count":7,"tag":"special"}"#);
        assert!(json.source_of(env.data.raw()));

        assert_eq!(env.data.value().count, 7);
        assert_eq!(env.data.value().tag, "special");
        // Tag inside decoded should also be zero-copy from the original json!
        assert!(json.source_of(&env.data.value().tag));

        // Deref to Inner
        assert_eq!(env.data.count, 7);
        assert_eq!(env.data.tag, "special");

        // into_parts
        let (raw_zc, inner) = env.data.into_parts();
        assert_eq!(raw_zc, r#"{"count":7,"tag":"special"}"#);
        assert_eq!(inner, Inner { count: 7, tag: ZCString::from("special") });

        // Direct constructor
        let direct_raw = ZCString::from(r#"{"count":12,"tag":"direct"}"#);
        let direct = ZCRawValueAnd::<Inner>::new(direct_raw).unwrap();
        assert_eq!(direct.count, 12);
        assert_eq!(direct.tag, "direct");

        // Clone, Debug, Display, PartialEq, Eq, PartialOrd, Ord, Hash, Default
        let cloned = direct.clone();
        assert_eq!(direct, cloned);
        assert_eq!(direct.cmp(&cloned), Ordering::Equal);

        let formatted = format!("{:?}", direct);
        assert!(formatted.contains("ZCRawValueAnd"));

        let default_val = ZCRawValueAnd::<u32>::default();
        assert_eq!(*default_val, 0);

        let mut hash_set = HashSet::new();
        hash_set.insert(direct);
        assert_eq!(hash_set.len(), 1);
    }

    #[test]
    fn test_zc_lazy_value() {
        #[derive(Deserialize, Serialize, Debug, PartialEq)]
        struct Expensive {
            numbers: Vec<i32>,
            name: ZCString,
        }

        #[derive(Deserialize, Serialize, Debug)]
        struct Doc {
            title: ZCString,
            expensive: ZCLazyValue<Expensive>,
        }

        let json = ZCString::from(r#"{"title":"Report","expensive":{"numbers":[10,20,30],"name":"Stats"}}"#);
        let doc: Doc = serde_json_from_zcstring(json.clone()).unwrap();

        assert_eq!(doc.title, "Report");
        assert_eq!(doc.expensive.raw().map(|v| v.as_str()), Some(r#"{"numbers":[10,20,30],"name":"Stats"}"#));
        assert!(json.source_of(doc.expensive.raw().unwrap()));

        // Before calling value(), parsing has not occurred
        assert!(!doc.expensive.is_decoded());
        assert!(!doc.expensive.is_value_only());

        // Calling value() decodes lazily
        let decoded = doc.expensive.value().unwrap();
        assert_eq!(decoded.numbers, vec![10, 20, 30]);
        assert_eq!(decoded.name, "Stats");
        assert!(doc.expensive.is_decoded());

        // Subsequent calls return the same Arc reference
        let second_access = doc.expensive.value().unwrap();
        assert!(Arc::ptr_eq(decoded, second_access));

        // Serialization outputs the raw JSON slice
        let serialized = serde_json::to_string(&doc).unwrap();
        assert_eq!(serialized, r#"{"title":"Report","expensive":{"numbers":[10,20,30],"name":"Stats"}}"#);
    }

    #[test]
    fn test_zc_lazy_value_parse_error() {
        #[derive(Deserialize, Serialize, Debug, PartialEq)]
        struct Target {
            val: u32,
        }

        #[derive(Deserialize, Debug)]
        struct Wrapper {
            lazy: ZCLazyValue<Target, false>,
        }

        // Pass invalid JSON format for Target
        let json = ZCString::from(r#"{"lazy":"not an object"}"#);
        let wrapper: Wrapper = serde_json_from_zcstring(json).unwrap();

        // Deserialization of Wrapper succeeds because lazy parsing is deferred!
        assert!(!wrapper.lazy.is_decoded());
        assert!(wrapper.lazy.value().is_err());
        assert!(wrapper.lazy.is_decoded());

        // Subsequent call returns cached error
        assert!(wrapper.lazy.value().is_err());
    }

    #[test]
    fn test_zc_lazy_value_constructors_and_default() {
        // from_raw
        let raw = ZCString::from("42");
        let l_raw = ZCLazyValue::<u32>::from_raw(raw);
        assert!(!l_raw.is_value_only());
        assert!(!l_raw.is_decoded());
        assert_eq!(**l_raw.value().unwrap(), 42);
        assert!(l_raw.is_decoded());

        // from_value
        let l_val = ZCLazyValue::<u32>::from_value(Arc::new(100));
        assert!(l_val.is_value_only());
        assert!(l_val.is_decoded());
        assert_eq!(l_val.raw(), None);
        assert_eq!(**l_val.value().unwrap(), 100);

        // from_error
        let json_err = serde_json::from_str::<u32>("invalid").unwrap_err();
        let l_err = ZCLazyValue::<u32>::from_error(Arc::new(json_err));
        assert!(l_err.is_value_only());
        assert!(l_err.value().is_err());

        // Default
        let l_def = ZCLazyValue::<String>::default();
        assert!(l_def.is_value_only());
        assert_eq!(l_def.value().unwrap().as_str(), "");

        // Serialization of value-only
        let serialized_val = serde_json::to_string(&l_val).unwrap();
        assert_eq!(serialized_val, "100");

        // Serialization of error returns Err
        let err_ser = serde_json::to_string(&l_err);
        assert!(err_ser.is_err());
    }

    #[test]
    fn test_zc_lazy_value_comparisons_and_hashing() {
        let v1 = ZCLazyValue::<u32>::from_value(Arc::new(50));
        let v2 = ZCLazyValue::<u32>::from_value(Arc::new(50));
        let v3 = ZCLazyValue::<u32>::from_value(Arc::new(75));

        assert_eq!(v1, v2);
        assert_ne!(v1, v3);
        assert!(v1 < v3);
        assert_eq!(v1.cmp(&v2), Ordering::Equal);
        assert_eq!(v1.cmp(&v3), Ordering::Less);

        // Errors equality & ordering
        let err1 = ZCLazyValue::<u32>::from_raw(ZCString::from("\"abc\""));
        let err2 = ZCLazyValue::<u32>::from_raw(ZCString::from("\"xyz\""));
        // Both fail parsing as u32, so both are Err
        assert_eq!(err1, err2);
        assert_eq!(err1.cmp(&err2), Ordering::Equal);

        // Value > Err
        assert!(v1 > err1);
        assert_eq!(v1.cmp(&err1), Ordering::Greater);
        assert_eq!(err1.cmp(&v1), Ordering::Less);

        // Hash
        let mut set = HashSet::new();
        set.insert(v1.clone());
        assert!(set.contains(&v2));

        let mut err_set = HashSet::new();
        err_set.insert(err1.clone());
        assert!(err_set.contains(&err2));

        // Debug formatting
        let debug_str = format!("{:?}", v1);
        assert!(debug_str.contains("ZCLazyValue"));
    }

    #[test]
    fn test_zc_lazy_value_concurrency() {
        let raw = ZCString::from(r#"{"items":[1,2,3,4,5]}"#);

        #[derive(Deserialize, Debug, PartialEq)]
        struct Data {
            items: Vec<i32>,
        }

        let lazy = Arc::new(ZCLazyValue::<Data>::from_raw(raw));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let lazy_clone = Arc::clone(&lazy);
            handles.push(thread::spawn(move || {
                let res = lazy_clone.value().unwrap();
                assert_eq!(res.items, vec![1, 2, 3, 4, 5]);
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert!(lazy.is_decoded());
    }

    #[test]
    fn test_zc_lazy_value_with_logging() {
        // When L = true, parse errors are logged via log::error!
        let raw = ZCString::from("not-a-number");
        let lazy_logging = ZCLazyValue::<u32, true>::from_raw(raw);
        assert!(lazy_logging.value().is_err());
    }

    #[test]
    fn test_read_range_boundary_conditions() {
        let mut data = Cursor::new(b"0123456789ABCDEF");

        // Included..=Included
        let r1 = ZCString::read_range(&mut data, 0..=4).unwrap();
        assert_eq!(r1, "01234");

        // Excluded..Excluded
        let r2 = ZCString::read_range(&mut data, (Bound::Excluded(2), Bound::Excluded(7))).unwrap();
        assert_eq!(r2, "3456");

        // Zero-length range at end of stream: stream_len..stream_len
        let r_eof = ZCString::read_range(&mut data, 16..16).unwrap();
        assert_eq!(r_eof, "");
        assert!(r_eof.is_empty());

        // Range beyond end of stream
        let err_past = ZCString::read_range(&mut data, 16..17);
        assert!(matches!(err_past, Err(ReaderError::InvalidRange { .. })));

        // Start beyond stream length
        let err_start_past = ZCString::read_range(&mut data, 20..20);
        assert!(matches!(err_start_past, Err(ReaderError::InvalidRange { .. })));

        // Excluded bound overflow check at u64::MAX (checked_add(1) is None)
        let err_overflow = ZCString::read_range(&mut data, (Bound::Excluded(u64::MAX), Bound::Unbounded));
        assert!(matches!(err_overflow, Err(ReaderError::InvalidRange { .. })));

        // Included bound overflow check at u64::MAX for end (checked_add(1) is None)
        let err_overflow2 = ZCString::read_range(&mut data, 0..=u64::MAX);
        assert!(matches!(err_overflow2, Err(ReaderError::InvalidRange { .. })));
    }

    #[test]
    fn test_read_range_utf8_split_error() {
        // "🦀" in UTF-8 is 4 bytes: [0xF0, 0x9F, 0xA6, 0x80]
        let crab = "🦀".as_bytes();
        let mut data = Cursor::new(crab);

        // Read only 2 bytes -> splits the 4-byte UTF-8 codepoint
        let res = ZCString::read_range(&mut data, 0..2);
        assert!(matches!(res, Err(ReaderError::Utf8(_))));
    }

    #[test]
    fn test_read_exact_multibyte_and_utf8_error() {
        let crab = "🦀🦀".as_bytes(); // 8 bytes
        let mut data = Cursor::new(crab);

        // Read first 4 bytes (exactly one crab emoji)
        let first_crab = ZCString::read_exact(&mut data, 4).unwrap();
        assert_eq!(first_crab, "🦀");

        // Read next 2 bytes (half a crab emoji) -> UTF-8 decoding error
        let res = ZCString::read_exact(&mut data, 2);
        assert!(matches!(res, Err(ReaderError::Utf8(_))));
    }

    #[test]
    fn test_source_of_precision_and_subtle_boundaries() {
        let parent = ZCString::from_str_without_source("0123456789");

        // Slices completely inside parent
        assert!(parent.source_of(&parent[0..5]));
        assert!(parent.source_of(&parent[5..10]));

        // Create a sub-slice: "2345" (offset 2, len 4)
        let sub = parent.substr(2..6);
        assert_eq!(sub, "2345");

        // A slice that starts at sub's address but extends past sub's end into parent:
        // &parent[2..8] starts at sub.as_ptr(), but len is 6 > sub.len() (4).
        // source_of must return false!
        let slice_extending_past = &parent[2..8];
        assert_eq!(slice_extending_past, "234567");
        assert!(!sub.source_of(slice_extending_past));

        // Empty slices at boundary positions
        assert!(sub.source_of(&sub[0..0]));
        assert!(sub.source_of(&sub[sub.len()..sub.len()]));

        // Slices from parent before sub's start
        assert!(!sub.source_of(&parent[0..2]));

        // Slices from parent after sub's end
        assert!(!sub.source_of(&parent[6..10]));

        // External string with identical text
        let stack_identical = "2345";
        assert!(!sub.source_of(stack_identical));
    }

    #[test]
    fn test_from_substr_boundary_and_external() {
        let root = ZCString::from("the quick brown fox");

        // Full slice of root
        let full = root.from_substr(&root[..]);
        assert_eq!(full, "the quick brown fox");
        assert!(root.source_of(&full));

        // Partial slice of root
        let partial = root.from_substr(&root[4..9]);
        assert_eq!(partial, "quick");
        assert!(root.source_of(&partial));

        // External string
        let external = "quick";
        let ext_zc = root.from_substr(external);
        assert_eq!(ext_zc, "quick");
        assert!(!root.source_of(&ext_zc));
    }

    #[test]
    fn test_substr_full_and_panics() {
        let s = ZCString::from("hello world");
        assert_eq!(s.substr(..), "hello world");
        assert_eq!(s.substr(..5), "hello");
        assert_eq!(s.substr(6..), "world");
        assert_eq!(s.substr(0..0), "");

        // Out of bounds panic
        let res_oob = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            s.substr(0..50);
        }));
        assert!(res_oob.is_err());

        // Split UTF-8 multibyte boundary panic
        let crab = ZCString::from("🦀");
        let res_utf8 = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crab.substr(0..2);
        }));
        assert!(res_utf8.is_err());
    }

    #[test]
    fn test_source_guard_unwind_safety() {
        let s1 = ZCString::from_str_without_source("outer source string");
        let s2 = ZCString::from_str_without_source("inner source string");

        let _guard1 = s1.get_source_guard();

        // Verify s1 is the active source
        let check1 = ZCString::from_str_with_source(&s1[0..5]);
        assert!(s1.source_of(&check1));

        // Simulate a panic inside an inner guard scope
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard2 = s2.get_source_guard();
            let check2 = ZCString::from_str_with_source(&s2[0..5]);
            assert!(s2.source_of(&check2));
            panic!("simulated panic");
        }));

        // After unwind, guard2's Drop executed; s1 should be restored!
        let check_restored = ZCString::from_str_with_source(&s1[0..5]);
        assert_eq!(check_restored, "outer");
        assert!(s1.source_of(&check_restored));
    }

    #[test]
    fn test_source_guard_sequential() {
        let s1 = ZCString::from("source one");
        let s2 = ZCString::from("source two");

        {
            let _g1 = s1.get_source_guard();
            let z1 = ZCString::from(&s1[0..6]);
            assert!(s1.source_of(&z1));
        }

        {
            let _g2 = s2.get_source_guard();
            let z2 = ZCString::from(&s2[0..6]);
            assert!(s2.source_of(&z2));
            assert!(!s1.source_of(&z2));
        }
    }

    #[test]
    fn test_with_source_panic_recovery() {
        let s1 = ZCString::from("outer source");
        let _guard = s1.get_source_guard();

        let s2 = ZCString::from("inner source");
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ZCString::with_source(s2.clone(), |_| {
                panic!("panic inside with_source closure");
            });
        }));

        // Outer source s1 must remain intact after inner with_source panic
        let check = ZCString::from_str_with_source(&s1[0..5]);
        assert_eq!(check, "outer");
        assert!(s1.source_of(&check));
    }

    #[test]
    fn test_zcstring_iter_wrapper_adapters() {
        let text = ZCString::from("cat,dog,,frog,bird,fish");

        // Wrapping split
        let tokens: Vec<ZCString> = text
            .wrap_iter(|s| s.split(','))
            .filter(|s| !s.is_empty())
            .take(3)
            .collect();

        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0], "cat");
        assert_eq!(tokens[1], "dog");
        assert_eq!(tokens[2], "frog");

        for token in &tokens {
            assert!(text.source_of(token));
        }
    }

    #[test]
    fn test_zc_raw_value_json_primitives_and_types() {
        #[derive(Deserialize, Serialize, Debug)]
        struct Primitives {
            num: ZCRawValue,
            str_val: ZCRawValue,
            bool_val: ZCRawValue,
            null_val: ZCRawValue,
            arr: ZCRawValue,
            obj: ZCRawValue,
        }

        let json = ZCString::from(r#"{"num":42,"str_val":"hello","bool_val":true,"null_val":null,"arr":[1,2],"obj":{"x":1}}"#);
        let parsed: Primitives = serde_json_from_zcstring(json.clone()).unwrap();

        assert_eq!(parsed.num.value(), "42");
        assert_eq!(parsed.str_val.value(), "\"hello\"");
        assert_eq!(parsed.bool_val.value(), "true");
        assert_eq!(parsed.null_val.value(), "null");
        assert_eq!(parsed.arr.value(), "[1,2]");
        assert_eq!(parsed.obj.value(), "{\"x\":1}");

        // All fields borrow zero-copy from the original json
        assert!(json.source_of(parsed.num.value()));
        assert!(json.source_of(parsed.str_val.value()));
        assert!(json.source_of(parsed.bool_val.value()));
        assert!(json.source_of(parsed.null_val.value()));
        assert!(json.source_of(parsed.arr.value()));
        assert!(json.source_of(parsed.obj.value()));

        // Re-serialization produces valid JSON
        let out = serde_json::to_string(&parsed).unwrap();
        let reparsed: Primitives = serde_json_from_zcstring(ZCString::from(out)).unwrap();
        assert_eq!(reparsed.num.value(), "42");
    }

    #[test]
    fn test_zc_raw_value_default_serialization_error() {
        // ZCRawValue::default() wraps an empty string ZCString::new() ("")
        let default_raw = ZCRawValue::default();
        assert_eq!(default_raw.value(), "");
        // "" is not valid JSON, so RawValue cannot parse it during serialization
        let res = serde_json::to_string(&default_raw);
        assert!(res.is_err());
    }

    #[test]
    fn test_zc_raw_value_and_comprehensive() {
        let raw_json = ZCString::from(r#"{"name":"widget","price":19.99}"#);
        #[derive(Deserialize, Serialize, Debug, PartialEq, Default)]
        struct Product {
            name: ZCString,
            price: f64,
        }

        // Direct construction
        let item = ZCRawValueAnd::<Product>::new(raw_json.clone()).unwrap();
        assert_eq!(item.raw(), r#"{"name":"widget","price":19.99}"#);
        assert_eq!(item.value().name, "widget");
        assert_eq!(item.value().price, 19.99);
        assert!(raw_json.source_of(&item.value().name));

        // Malformed JSON syntax passed to new
        let bad_json = ZCString::from("not valid json");
        let err = ZCRawValueAnd::<Product>::new(bad_json);
        assert!(err.is_err());

        // Schema mismatch (valid JSON, wrong type for Product)
        let mismatch_json = ZCString::from(r#"{"name": 12345}"#);
        let err2 = ZCRawValueAnd::<Product>::new(mismatch_json);
        assert!(err2.is_err());

        // Default serialization error (default raw is "")
        let default_item = ZCRawValueAnd::<Product>::default();
        assert!(serde_json::to_string(&default_item).is_err());
    }

    #[test]
    fn test_zc_lazy_value_multithreaded_contention() {
        let json = ZCString::from(r#"{"numbers":[1,2,3,4,5,6,7,8,9,10]}"#);
        #[derive(Deserialize, Debug, PartialEq)]
        struct Nums {
            numbers: Vec<i32>,
        }
        let lazy = Arc::new(ZCLazyValue::<Nums>::from_raw(json));

        let handles: Vec<_> = (0..16)
            .map(|_| {
                let l = Arc::clone(&lazy);
                thread::spawn(move || {
                    let res = l.value().unwrap();
                    assert_eq!(res.numbers.len(), 10);
                    assert_eq!(res.numbers[0], 1);
                    assert_eq!(res.numbers[9], 10);
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
        assert!(lazy.is_decoded());
    }

    #[test]
    fn test_zc_lazy_value_serialization_all_variants() {
        // 1. from_raw with valid JSON
        let l_raw = ZCLazyValue::<u32>::from_raw(ZCString::from("999"));
        assert_eq!(serde_json::to_string(&l_raw).unwrap(), "999");

        // 2. from_value
        let l_val = ZCLazyValue::<u32>::from_value(Arc::new(777));
        assert_eq!(serde_json::to_string(&l_val).unwrap(), "777");

        // 3. from_error
        let json_err = serde_json::from_str::<u32>("bad").unwrap_err();
        let l_err = ZCLazyValue::<u32>::from_error(Arc::new(json_err));
        assert!(serde_json::to_string(&l_err).is_err());

        // 4. Default
        let l_def = ZCLazyValue::<u32>::default();
        assert_eq!(serde_json::to_string(&l_def).unwrap(), "0");
    }

    #[test]
    fn test_zc_lazy_value_with_option_and_vec() {
        #[derive(Deserialize, Serialize, Debug)]
        struct Container {
            opt: Option<ZCLazyValue<u32>>,
            list: Vec<ZCLazyValue<String>>,
        }

        let json = ZCString::from(r#"{"opt":42,"list":["alpha","beta"]}"#);
        let container: Container = serde_json_from_zcstring(json).unwrap();

        assert!(container.opt.is_some());
        assert_eq!(**container.opt.unwrap().value().unwrap(), 42);
        assert_eq!(container.list.len(), 2);
        assert_eq!(container.list[0].value().unwrap().as_str(), "alpha");
        assert_eq!(container.list[1].value().unwrap().as_str(), "beta");
    }

    #[test]
    fn test_serde_json_deeply_nested_and_escaped() {
        #[derive(Deserialize, Debug)]
        struct Nested {
            plain: ZCString,
            escaped: ZCString,
            children: Vec<Child>,
        }

        #[derive(Deserialize, Debug)]
        struct Child {
            tag: ZCString,
            desc: ZCString,
        }

        let json_str = r#"{
            "plain": "plain_text",
            "escaped": "escaped\ntext",
            "children": [
                {"tag": "t1", "desc": "first child"},
                {"tag": "t2", "desc": "second\tchild"}
            ]
        }"#;
        let json = ZCString::from(json_str);
        let doc: Nested = serde_json_from_zcstring(json.clone()).unwrap();

        // Plain strings are zero copy
        assert_eq!(doc.plain, "plain_text");
        assert!(json.source_of(&doc.plain));

        assert_eq!(doc.children[0].tag, "t1");
        assert!(json.source_of(&doc.children[0].tag));

        assert_eq!(doc.children[0].desc, "first child");
        assert!(json.source_of(&doc.children[0].desc));

        assert_eq!(doc.children[1].tag, "t2");
        assert!(json.source_of(&doc.children[1].tag));

        // Escaped strings require unescaping, so they allocate independent buffers
        assert_eq!(doc.escaped, "escaped\ntext");
        assert!(!json.source_of(&doc.escaped));

        assert_eq!(doc.children[1].desc, "second\tchild");
        assert!(!json.source_of(&doc.children[1].desc));
    }

    #[test]
    fn test_type_send_sync_markers() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}

        assert_send::<ZCString>();
        assert_sync::<ZCString>();
        assert_send::<ZCRawValue>();
        assert_sync::<ZCRawValue>();
        assert_send::<ZCRawValueAnd<String>>();
        assert_sync::<ZCRawValueAnd<String>>();
        assert_send::<ZCLazyValue<String>>();
        assert_sync::<ZCLazyValue<String>>();
        assert_send::<ReaderError>();
        assert_sync::<ReaderError>();
    }

    #[test]
    fn test_zc_lazy_value_deferred_decode_contained_in_original_source() {
        // 1. Direct decode of ZCLazyValue into a ZCString
        #[derive(Deserialize, Debug)]
        struct DirectDoc {
            prefix: ZCString,
            lazy_str: ZCLazyValue<ZCString>,
        }

        let original_json = ZCString::from(
            r#"{"prefix":"initial","lazy_str":"deferred_string_content"}"#,
        );

        let doc: DirectDoc = serde_json_from_zcstring(original_json.clone()).unwrap();

        // Verify that prefix is zero-copy from original_json
        assert_eq!(doc.prefix, "initial");
        assert!(original_json.source_of(&doc.prefix));

        // lazy_str has not been decoded yet
        assert!(!doc.lazy_str.is_decoded());

        // Verify the raw JSON slice is inside original_json
        let raw_slice = doc.lazy_str.raw().unwrap();
        assert_eq!(raw_slice, "\"deferred_string_content\"");
        assert!(original_json.source_of(raw_slice));

        // Perform the deferred decode. Note that the outer serde_json_from_zcstring
        // scope has already completed and dropped its SourceGuard!
        let decoded_str = doc.lazy_str.value().unwrap();
        assert_eq!(decoded_str.as_str(), "deferred_string_content");
        assert!(doc.lazy_str.is_decoded());

        // PROVE that the deferred decode of ZCLazyValue into a ZCString
        // is in fact physically contained within the original source ZCString:
        assert!(original_json.source_of(decoded_str.as_str()));
        // And also contained within the intermediate raw JSON slice:
        assert!(raw_slice.source_of(decoded_str.as_str()));

        // 2. Deferred decode of ZCLazyValue into a struct containing multiple ZCString fields
        #[derive(Deserialize, Debug)]
        struct Payload {
            user: ZCString,
            role: ZCString,
        }

        #[derive(Deserialize, Debug)]
        struct Envelope {
            id: u64,
            payload: ZCLazyValue<Payload>,
        }

        let env_json = ZCString::from(
            r#"{"id":42,"payload":{"user":"ferris","role":"crab_developer"}}"#,
        );
        let envelope: Envelope = serde_json_from_zcstring(env_json.clone()).unwrap();

        assert_eq!(envelope.id, 42);
        assert!(!envelope.payload.is_decoded());

        // Trigger deferred decode
        let payload = envelope.payload.value().unwrap();
        assert!(envelope.payload.is_decoded());

        assert_eq!(payload.user, "ferris");
        assert_eq!(payload.role, "crab_developer");

        // PROVE that each ZCString inside the deferred decoded struct
        // is in fact physically contained within the original source ZCString:
        assert!(env_json.source_of(&payload.user));
        assert!(env_json.source_of(&payload.role));
    }
}
