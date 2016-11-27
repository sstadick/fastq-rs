use std::io::{Result, Read, Error, ErrorKind, Cursor};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;
use std::rc::Rc;


struct BufferMessage {
    buffer: Box<[u8]>,
    written: Result<usize>,
}


fn buffer_channel(bufsize: usize, queuelen: usize) -> (BufferSender, BufferReceiver) {
    assert!(queuelen >= 1);
    let (full_send, full_recv) = sync_channel(queuelen);
    let (empty_send, empty_recv) = sync_channel(queuelen);
    for _ in 0..queuelen {
        empty_send.send(vec![0u8; bufsize].into_boxed_slice()).unwrap();
    }

    let buffer_sender = BufferSender { full_send: full_send, empty_recv: empty_recv };
    let buffer_receiver = BufferReceiver { full_recv: full_recv, empty_send: Rc::new(empty_send) };
    (buffer_sender, buffer_receiver)
}


struct BufferSender {
    full_send: SyncSender<BufferMessage>,
    empty_recv: Receiver<Box<[u8]>>,
}


impl BufferSender {
    fn serve<F>(&mut self, mut func: F) where F: FnMut(Box<[u8]>) -> BufferMessage {
        while let Ok(buffer) = self.empty_recv.recv() {
            let reply = func(buffer);
            if self.full_send.send(reply).is_err() {
                break
            }
        }
    }
}


struct BufferReceiver {
    full_recv: Receiver<BufferMessage>,
    empty_send: Rc<SyncSender<Box<[u8]>>>,
}


impl BufferReceiver {
    fn next_reader(&mut self) -> Result<PartialReader> {
        let data = self.full_recv.recv().map_err(|e| Error::new(ErrorKind::BrokenPipe, e))?;
        Ok(PartialReader {
            available: data.written?,
            written: 0,
            sender: self.empty_send.clone(),
            data: Some(data.buffer),
        })
    }
}


struct PartialReader {
    sender: Rc<SyncSender<Box<[u8]>>>,
    data: Option<Box<[u8]>>,
    available: usize,
    written: usize,
}


impl Drop for PartialReader {
    fn drop(&mut self) {
        if let Some(data) = self.data.take() {
            // An error indicates that the other end has hung up.
            // In this case we don't need to do anything.
            let _ = self.sender.send(data);
        }
    }
}


impl Read for PartialReader {
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        let data = &self.data.as_ref().unwrap()[self.written..self.available];
        let res = Cursor::new(data).read(buffer)?;
        self.written += res;
        Ok(res)
    }
}


impl PartialReader {
    fn finished(&self) -> bool {
        self.available == self.written
    }
}


pub struct ThreadReader {
    pub handle: thread::JoinHandle<()>,
    receiver: BufferReceiver,
    reader: Option<PartialReader>
}


impl Read for ThreadReader {
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        if self.reader.is_none() || self.reader.as_ref().unwrap().finished() {
            // We drop the old partial reader manually before waiting for a new one
            // to prevent a deadlock if the queuelen is 1.
            ::std::mem::drop(self.reader.take());
            self.reader = Some(self.receiver.next_reader()?)
        }

        let reader = self.reader.as_mut().unwrap();
        reader.read(buffer)
    }
}


impl ThreadReader {
    pub fn new<R>(mut reader: R, buffsize: usize, queuelen: usize) -> ThreadReader
        where R: Read + Send + 'static
    {
        let (mut bufsend, bufrecv) = buffer_channel(buffsize, queuelen);
        let handle = thread::Builder::new().name("reader-thread".into()).spawn(move || {
            bufsend.serve(|mut buffer| {
                BufferMessage {
                    written: reader.read(&mut buffer),
                    buffer: buffer,
                }
            })
        }).unwrap();

        ThreadReader {
            handle: handle,
            receiver: bufrecv,
            reader: None,
        }
    }
}
