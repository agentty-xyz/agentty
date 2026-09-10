use std::fmt::Debug;
use std::io::{self, Write};

/// Checks that an example propagates a disconnected sink at every output write.
pub(super) fn assert_output_failures<Error: Debug>(
    mut run: impl FnMut(&mut WriteBudget) -> Result<(), Error>,
) {
    let mut complete = WriteBudget::new(usize::MAX);
    run(&mut complete).expect("successful example output");
    for allowed_writes in 0..complete.writes {
        let mut output = WriteBudget::new(allowed_writes);
        assert!(
            run(&mut output).is_err(),
            "failure after {allowed_writes} writes was lost"
        );
    }
}

/// Output sink that fails after a configurable number of successful writes.
pub(super) struct WriteBudget {
    remaining: usize,
    writes: usize,
}

impl WriteBudget {
    fn new(remaining: usize) -> Self {
        Self {
            remaining,
            writes: 0,
        }
    }
}

impl Write for WriteBudget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "disconnected output",
            ));
        }
        self.remaining -= 1;
        self.writes += 1;

        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
