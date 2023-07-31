use std::{
    env,
    io::{self, BufWriter, Read, Write},
    process::{Command, ExitCode, Stdio},
    sync::mpsc,
    thread::{self, JoinHandle},
};

const USAGE: &str = "Usage: crlfify-stdio <cmd> [args]";

macro_rules! log {
    ($($arg:tt)*) => {
        println!("[crlfify-stdio] {}", format!($($arg)*));
    }
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(cmd) = args.next() else {
        eprintln!("{}", USAGE);
        return ExitCode::FAILURE;
    };
    let args: Vec<String> = args.collect();

    let mut cmd = match Command::new(cmd)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(cmd) => cmd,
        Err(err) => {
            log!("Failed to spawn command: {}", err);
            return ExitCode::FAILURE;
        }
    };

    let cmd_stdout = cmd.stdout.take().unwrap();
    let cmd_stderr = cmd.stderr.take().unwrap();

    let (exit_tx, exit_rx) = mpsc::channel();

    spawn_forwarder(exit_tx.clone(), cmd_stdout, || io::stdout().lock());
    spawn_forwarder(exit_tx, cmd_stderr, || io::stderr().lock());

    exit_rx.recv().expect("exit channel closed unexpectedly")
}

fn spawn_forwarder<Source, GetSink, Sink>(
    exit: mpsc::Sender<ExitCode>,
    mut source: Source,
    sink: GetSink,
) -> JoinHandle<()>
where
    Source: Read + Send + 'static,
    GetSink: FnOnce() -> Sink + Send + 'static,
    Sink: Write + 'static,
{
    thread::spawn(move || {
        let mut sink = BufWriter::new(sink());
        let mut buf = [0; 1024];

        let exit_code = 'main: loop {
            let count = match source.read(&mut buf) {
                Ok(0) => break ExitCode::SUCCESS,
                Ok(count) => count,
                Err(err) => match err.kind() {
                    io::ErrorKind::Interrupted => continue,
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe => {
                        break ExitCode::SUCCESS
                    }
                    _ => {
                        log!("Read error: {}", err);
                        break ExitCode::FAILURE;
                    }
                },
            };

            let bytes = &buf[0..count];

            let mut prev = None;
            for &byte in bytes {
                if byte == b'\n' && prev != Some(b'\r') {
                    if let Err(err) = sink.write_all(&[b'\r']) {
                        if err.kind() == io::ErrorKind::BrokenPipe {
                            break 'main ExitCode::SUCCESS;
                        } else {
                            log!("Write error: {}", err);
                            break 'main ExitCode::FAILURE;
                        }
                    }
                }

                if let Err(err) = sink.write_all(&[byte]) {
                    if err.kind() == io::ErrorKind::BrokenPipe {
                        break 'main ExitCode::SUCCESS;
                    } else {
                        log!("Write error: {}", err);
                        break 'main ExitCode::FAILURE;
                    }
                }

                if let Err(err) = sink.flush() {
                    if err.kind() == io::ErrorKind::BrokenPipe {
                        break 'main ExitCode::SUCCESS;
                    } else {
                        log!("Write error: {}", err);
                        break 'main ExitCode::FAILURE;
                    }
                }

                prev = Some(byte);
            }
        };

        let _ = sink.flush();
        let _ = exit.send(exit_code);
    })
}
