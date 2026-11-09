//! What X programs send, read as it passes through the door: the stream
//! cut into requests, those wayluck wants to know about picked out, and
//! those no program may make turned into one that fails. The rest is only
//! counted past.

use super::struts::Strut;

/// What a program's requests tell wayluck.
#[derive(Debug, PartialEq)]
pub enum Heard {
    /// A window's strut set (Some) or taken away (None).
    Strut(u32, Option<Strut>),
}

/// What the door knows of Xwayland: its numbers for the atoms it reads
/// requests for, and for the extensions whose requests it turns down.
#[derive(Clone, Copy, Debug)]
pub struct Known {
    pub strut: u32,
    pub strut_partial: u32,
    /// The SECURITY extension's major opcode, if Xwayland has it.
    pub security: Option<u8>,
}

const GET_GEOMETRY: u8 = 14;
const CHANGE_PROPERTY: u8 = 18;
const DELETE_PROPERTY: u8 = 19;
/// Who may connect: a program let in by the door mustn't let others in.
const CHANGE_HOSTS: u8 = 109;
const SET_ACCESS_CONTROL: u8 = 111;
/// SECURITY's request minting a cookie (one more way in).
const GENERATE_AUTHORIZATION: u8 = 1;

/// One program's requests, read as they come (after its setup).
pub struct Requests {
    known: Known,
    /// Its byte order: little end first?
    little: bool,
    /// What's come of a request not yet passed on: until it's known what
    /// it is, or (one to read) until it's whole.
    pending: Vec<u8>,
    /// Bytes left of the request being passed over: on to Xwayland, or
    /// (one turned down) dropped.
    skip: usize,
    dropping: bool,
}

/// What's to be done with a request.
enum Fate {
    /// On as it is, unread.
    Pass,
    /// On, once whole, and read.
    Read,
    /// Instead, one that fails: the program hears an error for it.
    Refuse,
}

impl Requests {
    pub fn new(known: Known, little: bool) -> Self {
        Self {
            known,
            little,
            pending: Vec::new(),
            skip: 0,
            dropping: false,
        }
    }

    /// More of the stream: what goes on to Xwayland is added to out, and
    /// what the requests it completes tell is returned.
    pub fn pass(&mut self, mut bytes: &[u8], out: &mut Vec<u8>) -> Vec<Heard> {
        let mut heard = Vec::new();
        loop {
            let skipped = self.skip.min(bytes.len());
            if !self.dropping {
                out.extend_from_slice(&bytes[..skipped]);
            }
            self.skip -= skipped;
            bytes = &bytes[skipped..];
            if self.skip > 0 {
                break;
            }
            self.pending.extend_from_slice(bytes);
            bytes = &[];
            let Some((length, fate)) = self.next() else {
                break;
            };
            let have = self.pending.len().min(length);
            match fate {
                Fate::Read if have < length => break,
                Fate::Read => {
                    let request: Vec<u8> = self.pending.drain(..length).collect();
                    heard.extend(self.read(&request));
                    out.extend(request);
                }
                Fate::Pass => {
                    out.extend(self.pending.drain(..have));
                    (self.skip, self.dropping) = (length - have, false);
                }
                Fate::Refuse => {
                    self.pending.drain(..have);
                    (self.skip, self.dropping) = (length - have, true);
                    out.extend(self.failing());
                }
            }
        }
        heard
    }

    /// The next request's length in bytes, and what's to be done with it:
    /// once enough of it's come to say.
    fn next(&self) -> Option<(usize, Fate)> {
        let (p, little) = (&self.pending, self.little);
        if p.len() < 4 {
            return None;
        }
        // In 4-byte units; 0: a big request's, after it.
        let length = match u16_at(p, 2, little) {
            0 if p.len() < 8 => return None,
            0 => u32_at(p, 4, little) as usize * 4,
            units => units as usize * 4,
        };
        let (major, minor) = (p[0], p[1]);
        let fate = match major {
            CHANGE_PROPERTY | DELETE_PROPERTY => Fate::Read,
            CHANGE_HOSTS | SET_ACCESS_CONTROL => Fate::Refuse,
            m if Some(m) == self.known.security && minor == GENERATE_AUTHORIZATION => Fate::Refuse,
            _ => Fate::Pass,
        };
        // A broken one (shorter than its header): nothing more's read.
        Some((length.max(4), fate))
    }

    /// A request in place of one turned down, the same one in the count X
    /// keeps of them, that fails: the geometry of no drawable.
    fn failing(&self) -> Vec<u8> {
        let mut request = vec![GET_GEOMETRY, 0];
        if self.little {
            request.extend(2u16.to_le_bytes());
        } else {
            request.extend(2u16.to_be_bytes());
        }
        request.extend([0; 4]);
        request
    }

    /// What a whole request tells, if anything.
    fn read(&self, r: &[u8]) -> Option<Heard> {
        let little = self.little;
        let k = self.known;
        let is_strut = |atom: u32| atom == k.strut || atom == k.strut_partial;
        match r[0] {
            // mode, window, property, type, format, its length (in
            // format's units), its data.
            CHANGE_PROPERTY if r.len() >= 24 => {
                let (window, property) = (u32_at(r, 4, little), u32_at(r, 8, little));
                if !is_strut(property) {
                    return None;
                }
                let replace = r[1] == 0;
                let (format, count) = (r[16], u32_at(r, 20, little) as usize);
                if !replace || format != 32 {
                    return None;
                }
                let values: Vec<u32> = (0..count)
                    .map(|i| 24 + i * 4)
                    .take_while(|at| at + 4 <= r.len())
                    .map(|at| u32_at(r, at, little))
                    .collect();
                Some(Heard::Strut(window, Strut::from_property(&values)))
            }
            DELETE_PROPERTY if r.len() >= 12 => {
                let (window, property) = (u32_at(r, 4, little), u32_at(r, 8, little));
                is_strut(property).then_some(Heard::Strut(window, None))
            }
            _ => None,
        }
    }
}

pub fn u16_at(bytes: &[u8], at: usize, little: bool) -> u16 {
    let b = [bytes[at], bytes[at + 1]];
    if little {
        u16::from_le_bytes(b)
    } else {
        u16::from_be_bytes(b)
    }
}

fn u32_at(bytes: &[u8], at: usize, little: bool) -> u32 {
    let b = [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
    if little {
        u32::from_le_bytes(b)
    } else {
        u32::from_be_bytes(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KNOWN: Known = Known {
        strut: 300,
        strut_partial: 301,
        security: Some(140),
    };

    /// NoOperation, 3 units long.
    const NOTHING: [u8; 12] = [127, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0];

    /// ChangeProperty: replace, window 7, _NET_WM_STRUT_PARTIAL, CARDINAL,
    /// 32-bit, 12 of them.
    fn strut() -> Vec<u8> {
        let values = [0, 0, 30, 0, 0, 0, 0, 0, 0, 1919, 0, 0];
        let mut r = vec![18, 0, (6 + values.len()) as u8, 0];
        for word in [7, 301, 6] {
            r.extend(u32::to_le_bytes(word));
        }
        r.extend([32, 0, 0, 0]);
        r.extend(u32::to_le_bytes(values.len() as u32));
        for v in values {
            r.extend(u32::to_le_bytes(v));
        }
        r
    }

    /// DeleteProperty: window 7, _NET_WM_STRUT_PARTIAL.
    fn unstrut() -> Vec<u8> {
        let mut r = vec![19, 0, 3, 0];
        r.extend(u32::to_le_bytes(7));
        r.extend(u32::to_le_bytes(301));
        r
    }

    /// SetAccessControl: disable (a request with no more to it).
    const OPEN_UP: [u8; 4] = [111, 0, 1, 0];

    /// SECURITY's GenerateAuthorization, its cookie's name and data and
    /// the rest (made up: only its length matters).
    fn mint() -> Vec<u8> {
        let mut r = vec![140, 1, 5, 0];
        r.extend([0; 16]);
        r
    }

    /// What's put in place of a request turned down.
    const FAILING: [u8; 8] = [14, 0, 2, 0, 0, 0, 0, 0];

    fn stream() -> Vec<u8> {
        [
            NOTHING.to_vec(),
            strut(),
            OPEN_UP.to_vec(),
            mint(),
            unstrut(),
            NOTHING.to_vec(),
        ]
        .concat()
    }

    fn passed() -> Vec<u8> {
        [
            NOTHING.to_vec(),
            strut(),
            FAILING.to_vec(),
            FAILING.to_vec(),
            unstrut(),
            NOTHING.to_vec(),
        ]
        .concat()
    }

    fn heard() -> Vec<Heard> {
        let strut = Strut::from_property(&[0, 0, 30, 0, 0, 0, 0, 0, 0, 1919, 0, 0]);
        vec![Heard::Strut(7, strut), Heard::Strut(7, None)]
    }

    #[test]
    fn struts_are_read_and_ways_in_turned_down() {
        let mut requests = Requests::new(KNOWN, true);
        let mut out = Vec::new();
        assert_eq!(requests.pass(&stream(), &mut out), heard());
        assert_eq!(out, passed());
    }

    #[test]
    fn however_the_stream_comes_cut_up() {
        let stream = stream();
        for size in 1..stream.len() {
            let mut requests = Requests::new(KNOWN, true);
            let mut out = Vec::new();
            let heard: Vec<Heard> = stream
                .chunks(size)
                .flat_map(|c| requests.pass(c, &mut out))
                .collect();
            assert_eq!(heard, self::heard(), "in pieces of {size}");
            assert_eq!(out, passed(), "in pieces of {size}");
        }
    }

    #[test]
    fn other_properties_tell_nothing() {
        let known = Known {
            strut: 1,
            strut_partial: 2,
            security: None,
        };
        let mut requests = Requests::new(known, true);
        let mut out = Vec::new();
        assert_eq!(
            requests.pass(&[NOTHING.to_vec(), strut()].concat(), &mut out),
            vec![]
        );
    }
}
