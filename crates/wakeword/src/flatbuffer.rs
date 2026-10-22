//! A minimal, bounds-checked `FlatBuffers` reader: just what reading a `.tflite` model needs
//! (tables with a vtable, scalars, strings, vectors of scalars and of tables). Every access is
//! checked, so a corrupt upload is an error, never a panic.

use crate::Error;

fn slice(buf: &[u8], at: usize, len: usize) -> Result<&[u8], Error> {
    at.checked_add(len)
        .and_then(|end| buf.get(at..end))
        .ok_or(Error::Malformed("offset out of bounds"))
}

fn u16_at(buf: &[u8], at: usize) -> Result<u16, Error> {
    Ok(u16::from_le_bytes(
        slice(buf, at, 2)?.try_into().expect("2 bytes"),
    ))
}

fn u32_at(buf: &[u8], at: usize) -> Result<u32, Error> {
    Ok(u32::from_le_bytes(
        slice(buf, at, 4)?.try_into().expect("4 bytes"),
    ))
}

fn i32_at(buf: &[u8], at: usize) -> Result<i32, Error> {
    Ok(i32::from_le_bytes(
        slice(buf, at, 4)?.try_into().expect("4 bytes"),
    ))
}

/// Follows a `uoffset_t` stored at `at`.
fn follow(buf: &[u8], at: usize) -> Result<usize, Error> {
    let offset = u32_at(buf, at)? as usize;
    at.checked_add(offset).ok_or(Error::Malformed("offset overflow"))
}

/// A table: an object with a vtable of optional fields.
#[derive(Debug, Clone, Copy)]
pub struct Table<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Table<'a> {
    /// The root table of a buffer.
    pub fn root(buf: &'a [u8]) -> Result<Self, Error> {
        Ok(Self {
            buf,
            at: follow(buf, 0)?,
        })
    }

    /// Where field `id` is stored, if it is present.
    fn field(&self, id: usize) -> Result<Option<usize>, Error> {
        let vtable = i64::try_from(self.at).map_err(|_| Error::Malformed("table offset"))?
            - i64::from(i32_at(self.buf, self.at)?);
        let vtable = usize::try_from(vtable).map_err(|_| Error::Malformed("vtable offset"))?;
        let len = usize::from(u16_at(self.buf, vtable)?);
        let entry = 4 + 2 * id;
        if entry + 2 > len {
            return Ok(None);
        }
        match u16_at(self.buf, vtable + entry)? {
            0 => Ok(None),
            offset => Ok(Some(self.at + usize::from(offset))),
        }
    }

    pub fn u8(&self, id: usize, default: u8) -> Result<u8, Error> {
        match self.field(id)? {
            Some(at) => Ok(slice(self.buf, at, 1)?[0]),
            None => Ok(default),
        }
    }

    pub fn bool(&self, id: usize) -> Result<bool, Error> {
        Ok(self.u8(id, 0)? != 0)
    }

    pub fn i32(&self, id: usize, default: i32) -> Result<i32, Error> {
        self.field(id)?.map_or(Ok(default), |at| i32_at(self.buf, at))
    }

    pub fn u32(&self, id: usize, default: u32) -> Result<u32, Error> {
        self.field(id)?.map_or(Ok(default), |at| u32_at(self.buf, at))
    }

    pub fn table(&self, id: usize) -> Result<Option<Table<'a>>, Error> {
        self.field(id)?
            .map(|at| {
                Ok(Table {
                    buf: self.buf,
                    at: follow(self.buf, at)?,
                })
            })
            .transpose()
    }

    pub fn vector(&self, id: usize) -> Result<Option<Vector<'a>>, Error> {
        self.field(id)?
            .map(|at| {
                let at = follow(self.buf, at)?;
                let len = u32_at(self.buf, at)? as usize;
                Ok(Vector {
                    buf: self.buf,
                    start: at + 4,
                    len,
                })
            })
            .transpose()
    }

    pub fn string(&self, id: usize) -> Result<Option<&'a str>, Error> {
        self.vector(id)?
            .map(|v| std::str::from_utf8(v.bytes()?).map_err(|_| Error::Malformed("string is not UTF-8")))
            .transpose()
    }
}

/// A vector; its elements are read by type.
#[derive(Debug, Clone, Copy)]
pub struct Vector<'a> {
    buf: &'a [u8],
    start: usize,
    len: usize,
}

impl<'a> Vector<'a> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn bytes(&self) -> Result<&'a [u8], Error> {
        slice(self.buf, self.start, self.len)
    }

    fn element(&self, i: usize, size: usize) -> Result<usize, Error> {
        if i >= self.len {
            return Err(Error::Malformed("vector index out of bounds"));
        }
        Ok(self.start + i * size)
    }

    pub fn table(&self, i: usize) -> Result<Table<'a>, Error> {
        let at = self.element(i, 4)?;
        Ok(Table {
            buf: self.buf,
            at: follow(self.buf, at)?,
        })
    }

    pub fn i32s(&self) -> Result<Vec<i32>, Error> {
        (0..self.len)
            .map(|i| i32_at(self.buf, self.element(i, 4)?))
            .collect()
    }

    pub fn f32s(&self) -> Result<Vec<f32>, Error> {
        (0..self.len)
            .map(|i| Ok(f32::from_bits(u32_at(self.buf, self.element(i, 4)?)?)))
            .collect()
    }

    pub fn i64s(&self) -> Result<Vec<i64>, Error> {
        (0..self.len)
            .map(|i| {
                let at = self.element(i, 8)?;
                Ok(i64::from_le_bytes(
                    slice(self.buf, at, 8)?.try_into().expect("8 bytes"),
                ))
            })
            .collect()
    }
}
