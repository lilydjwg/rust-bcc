use bcc_sys::bccapi::*;
use byteorder::{NativeEndian, WriteBytesExt};
use failure::*;

use std::io::Cursor;

use crate::cpuonline;
use crate::table::Table;
use crate::types::*;

const BPF_PERF_READER_PAGE_CNT: i32 = 64;

unsafe extern "C" fn raw_callback(pc: MutPointer, ptr: MutPointer, size: i32) {
    let slice = std::slice::from_raw_parts(ptr as *const u8, size as usize);
    // prevent unwinding into C code
    // no custom panic hook set, panic will be printed as is
    let _ = std::panic::catch_unwind(|| {
      let raw_cb = std::mem::transmute::<_, fn(&[u8])>(pc);
      raw_cb(slice)
    });
}

// need this to be represented in memory as just a pointer!!
// very important!!
#[repr(C)]
struct PerfReader {
    ptr: *mut perf_reader,
}

impl PerfReader {
    pub fn fd(&mut self) -> i32 {
        unsafe { perf_reader_fd(self.ptr) }
    }
}

impl Drop for PerfReader {
    fn drop(&mut self) {
        unsafe { perf_reader_free(self.ptr as MutPointer) }
    }
}

#[allow(dead_code)]
pub struct PerfMap {
    // table and callbacks are just in here to make sure the data stays owned/alive
    // TODO: improve this API
    table: Table,
    readers: Vec<PerfReader>,
}

pub fn init_perf_map(mut table: Table, cb: fn(&[u8])) -> Result<PerfMap, Error> {
    let fd = table.fd();
    let key_size = table.key_size();
    let leaf_size = table.leaf_size();
    let mut key = vec![0; key_size];
    let leaf = vec![0; leaf_size];

    if key_size != 4 || leaf_size != 4 {
        return Err(format_err!("passed table has wrong size"));
    }

    let mut readers: Vec<PerfReader> = vec![];
    let mut cur = Cursor::new(leaf);

    let cpus = cpuonline::get()?;
    for (i, cpu) in cpus.iter().enumerate() {
        unsafe {
            let mut reader = open_perf_buffer(*cpu, cb)?;
            let perf_fd = reader.fd() as u32;
            readers.push(reader);

            cur.write_u32::<NativeEndian>(perf_fd)?;
            table
                .set(&mut key, &mut cur.get_mut())
                .context("Unable to initialize perf map")?;
            if i < cpus.len() - 1 {
              let r = bpf_get_next_key(
                  fd,
                  key.as_mut_ptr() as MutPointer,
                  key.as_mut_ptr() as MutPointer,
              );
              if r != 0 {
                  return Err(format_err!("todo: oh no"));
              }
            }
            cur.set_position(0);
        }
    }
    Ok(PerfMap {
        table,
        readers,
    })
}

impl PerfMap {
    pub fn poll(&mut self, timeout: i32) {
        unsafe {
            perf_reader_poll(
                self.readers.len() as i32,
                self.readers.as_ptr() as *mut *mut perf_reader,
                timeout,
            )
        };
    }
}

fn open_perf_buffer(
    cpu: usize,
    raw_cb: fn(&[u8]),
) -> Result<PerfReader, Error> {
    let reader = unsafe {
        bpf_open_perf_buffer(
            Some(raw_callback),
            None,
            raw_cb as MutPointer,
            -1, /* pid */
            cpu as i32,
            BPF_PERF_READER_PAGE_CNT,
        )
    };
    if reader.is_null() {
        return Err(format_err!("failed to open perf buffer"));
    }
    Ok(PerfReader {
      ptr: reader as *mut perf_reader,
    })
}
