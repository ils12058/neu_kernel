use super::*;
use crate::node::Nodes;

fn pair() -> (Nodes, Objects, [NodeId; 2], [[Handle; 8]; 2]) {
    let mut n = Nodes::new();
    let ids = [n.create().unwrap(), n.create().unwrap()];
    let mut o = Objects::with_channels();
    for (side, node) in ids.into_iter().enumerate() {
        let flag = o.flag(side == 0).unwrap();
        for index in 0..4 {
            n.authority_mut(node)
                .unwrap()
                .install_bootstrap(&mut o, &flag, Rights::READ, index)
                .unwrap();
        }
        o.release(flag).unwrap();
    }
    n.bootstrap_channel(&mut o, ids[0], ids[1]).unwrap();
    for node in ids {
        let t = n.authority_mut(node).unwrap();
        t.configure_moves().unwrap();
        t.seal_bootstrap();
    }
    let handles = ids.map(|node| {
        core::array::from_fn(|i| {
            n.authority(node)
                .unwrap()
                .bootstrap_get(&o, node, i as u64)
                .unwrap()
        })
    });
    (n, o, ids, handles)
}
fn conserved(o: &Objects) {
    let s = o.channel_state().snapshot();
    for side in 0..2 {
        assert_eq!(s[16 + side], s[19 - side] + s[13 - side * 5] + s[21 - side]);
    }
}
fn read(n: &Nodes, o: &mut Objects, node: NodeId, h: Handle) -> Result<u64> {
    n.authority(node)
        .unwrap()
        .lookup(o, node, h, ObjectType::Flag, Rights::READ)?
        .read()
}

#[test]
fn move_owns_exact_reference_preserves_rights_and_installs_fresh_receiver_handle() {
    let (mut n, mut o, [a, b], h) = pair();
    let id = n.authority(a).unwrap().describe(a, h[0][3]).unwrap().0;
    let before = o.stats();
    n.authority_mut(a)
        .unwrap()
        .send_cap(&mut o, a, h[0][4], h[0][3])
        .unwrap();
    assert_eq!(read(&n, &mut o, a, h[0][3]), Err(Error::Stale));
    assert_eq!(
        n.authority_mut(a).unwrap().close(&mut o, a, h[0][3]),
        Err(Error::Stale)
    );
    assert_eq!(o.stats(), before);
    conserved(&o);
    assert_eq!(
        n.authority_mut(b).unwrap().receive_cap(&mut o, b, h[1][4]),
        Err(Error::Capacity)
    );
    n.authority_mut(b)
        .unwrap()
        .close(&mut o, b, h[1][2])
        .unwrap();
    let received = n
        .authority_mut(b)
        .unwrap()
        .receive_cap(&mut o, b, h[1][4])
        .unwrap();
    assert_ne!(received, h[0][3]);
    assert_ne!(received, h[1][2]);
    assert_eq!(
        n.authority(b).unwrap().describe(b, received),
        Ok((id, Rights::READ))
    );
    assert_eq!(read(&n, &mut o, b, received), Ok(1));
    assert_eq!(
        n.authority(b)
            .unwrap()
            .lookup(&mut o, b, received, ObjectType::Flag, Rights::WRITE)
            .err(),
        Some(Error::Rights)
    );
    n.authority_mut(b)
        .unwrap()
        .close(&mut o, b, received)
        .unwrap();
    assert_eq!(read(&n, &mut o, a, h[0][0]), Ok(1)); // Independent reference unchanged.
    conserved(&o);
}

#[test]
fn move_send_failures_preserve_source_queue_references_and_generations() {
    let (mut n, mut o, [a, b], h) = pair();
    for (caller, endpoint, source, error) in [
        (a, h[0][4], Handle::from_raw(0), Error::Handle),
        (a, h[0][4], Handle::from_raw(0x200000004), Error::Stale),
        (b, h[0][4], h[0][3], Error::Owner),
        (a, h[0][5], h[0][3], Error::Rights),
        (a, h[0][4], h[0][0], Error::Transfer),
        (a, h[0][4], h[0][6], Error::Type),
    ] {
        let stats = o.stats();
        let q = o.channel_state().snapshot();
        let tags = o.channel_state().transfer_snapshot();
        let gens = n
            .authority(a)
            .unwrap()
            .slots
            .each_ref()
            .map(|s| s.generation);
        assert_eq!(
            n.authority_mut(a)
                .unwrap()
                .send_cap(&mut o, caller, endpoint, source),
            Err(error)
        );
        assert_eq!(o.stats(), stats);
        assert_eq!(o.channel_state().snapshot(), q);
        assert_eq!(o.channel_state().transfer_snapshot(), tags);
        assert_eq!(
            n.authority(a)
                .unwrap()
                .slots
                .each_ref()
                .map(|s| s.generation),
            gens
        );
        assert_eq!(read(&n, &mut o, a, h[0][3]), Ok(1));
    }
}

#[test]
fn full_and_closed_peer_do_not_detach_source_and_retry_commits_once() {
    let (mut n, mut o, [a, b], h) = pair();
    for value in [0, u64::MAX] {
        n.authority(a)
            .unwrap()
            .lookup(
                &mut o,
                a,
                h[0][4],
                ObjectType::ChannelEndpoint,
                Rights::SEND,
            )
            .unwrap()
            .channel_send(value)
            .unwrap();
    }
    let q = o.channel_state().snapshot();
    let stats = o.stats();
    assert_eq!(
        n.authority_mut(a)
            .unwrap()
            .send_cap(&mut o, a, h[0][4], h[0][3]),
        Err(Error::Full)
    );
    assert_eq!(o.channel_state().snapshot(), q);
    assert_eq!(o.stats(), stats);
    assert_eq!(read(&n, &mut o, a, h[0][3]), Ok(1));
    assert_eq!(
        n.authority(b)
            .unwrap()
            .lookup(
                &mut o,
                b,
                h[1][4],
                ObjectType::ChannelEndpoint,
                Rights::RECEIVE
            )
            .unwrap()
            .channel_receive(),
        Ok(0)
    );
    n.authority_mut(a)
        .unwrap()
        .send_cap(&mut o, a, h[0][4], h[0][3])
        .unwrap();
    assert_eq!(
        n.authority_mut(a)
            .unwrap()
            .send_cap(&mut o, a, h[0][4], h[0][3]),
        Err(Error::Stale)
    );
    n.authority_mut(b).unwrap().drain(&mut o, b).unwrap();
    assert_eq!(o.channel_state().snapshot()[21], 2);
    conserved(&o);
    assert_eq!(
        n.authority_mut(a)
            .unwrap()
            .send_cap(&mut o, a, h[0][4], h[0][0]),
        Err(Error::Transfer)
    );
    n.authority_mut(a).unwrap().drain(&mut o, a).unwrap();
    assert_eq!(o.stats(), Stats::default());
}

#[test]
fn mixed_fifo_never_skips_or_reinterprets_capability_and_scalar_heads() {
    let (mut n, mut o, [a, b], h) = pair();
    n.authority_mut(b)
        .unwrap()
        .close(&mut o, b, h[1][2])
        .unwrap();
    n.authority(a)
        .unwrap()
        .lookup(
            &mut o,
            a,
            h[0][4],
            ObjectType::ChannelEndpoint,
            Rights::SEND,
        )
        .unwrap()
        .channel_send(11)
        .unwrap();
    n.authority_mut(a)
        .unwrap()
        .send_cap(&mut o, a, h[0][4], h[0][3])
        .unwrap();
    let q = o.channel_state().snapshot();
    assert_eq!(
        n.authority_mut(b).unwrap().receive_cap(&mut o, b, h[1][4]),
        Err(Error::MessageKind)
    );
    assert_eq!(o.channel_state().snapshot(), q);
    assert_eq!(
        n.authority(b)
            .unwrap()
            .lookup(
                &mut o,
                b,
                h[1][4],
                ObjectType::ChannelEndpoint,
                Rights::RECEIVE
            )
            .unwrap()
            .channel_receive(),
        Ok(11)
    );
    n.authority(a)
        .unwrap()
        .lookup(
            &mut o,
            a,
            h[0][4],
            ObjectType::ChannelEndpoint,
            Rights::SEND,
        )
        .unwrap()
        .channel_send(22)
        .unwrap();
    let q = o.channel_state().snapshot();
    let tags = o.channel_state().transfer_snapshot();
    let stats = o.stats();
    assert_eq!(
        n.authority(b)
            .unwrap()
            .lookup(
                &mut o,
                b,
                h[1][4],
                ObjectType::ChannelEndpoint,
                Rights::RECEIVE
            )
            .unwrap()
            .channel_receive(),
        Err(Error::MessageKind)
    );
    assert_eq!(o.channel_state().snapshot(), q);
    assert_eq!(o.channel_state().transfer_snapshot(), tags);
    assert_eq!(o.stats(), stats);
    let received = n
        .authority_mut(b)
        .unwrap()
        .receive_cap(&mut o, b, h[1][4])
        .unwrap();
    assert_eq!(read(&n, &mut o, b, received), Ok(1));
    assert_eq!(
        n.authority(b)
            .unwrap()
            .lookup(
                &mut o,
                b,
                h[1][4],
                ObjectType::ChannelEndpoint,
                Rights::RECEIVE
            )
            .unwrap()
            .channel_receive(),
        Ok(22)
    );
    conserved(&o);
}

#[test]
fn sender_terminal_cleanup_preserves_queue_and_receiver_terminal_cleanup_releases_it() {
    for receiver_dies in [false, true] {
        let (mut n, mut o, [a, b], h) = pair();
        let id = n.authority(a).unwrap().describe(a, h[0][3]).unwrap().0;
        n.authority_mut(a)
            .unwrap()
            .send_cap(&mut o, a, h[0][4], h[0][3])
            .unwrap();
        n.authority_mut(a).unwrap().terminate();
        n.authority_mut(a).unwrap().drain(&mut o, a).unwrap();
        assert_eq!(o.info(id).unwrap().references, 1);
        assert_eq!(o.stats().pins, 0);
        if !receiver_dies {
            n.authority_mut(b)
                .unwrap()
                .close(&mut o, b, h[1][2])
                .unwrap();
            let received = n
                .authority_mut(b)
                .unwrap()
                .receive_cap(&mut o, b, h[1][4])
                .unwrap();
            assert_eq!(read(&n, &mut o, b, received), Ok(1));
        }
        n.authority_mut(b).unwrap().terminate();
        n.authority_mut(b).unwrap().drain(&mut o, b).unwrap();
        assert_eq!(o.info(id), Err(Error::Object));
        assert_eq!(o.stats(), Stats::default());
        conserved(&o);
    }
}

#[test]
fn receiver_generation_exhaustion_preserves_head_and_queued_reference() {
    let (mut n, mut o, [a, b], h) = pair();
    n.authority_mut(a)
        .unwrap()
        .send_cap(&mut o, a, h[0][4], h[0][3])
        .unwrap();
    n.authority_mut(b)
        .unwrap()
        .close(&mut o, b, h[1][2])
        .unwrap();
    n.authority_mut(b).unwrap().slots[2].generation = u32::MAX;
    let q = o.channel_state().snapshot();
    let stats = o.stats();
    assert_eq!(
        n.authority_mut(b).unwrap().receive_cap(&mut o, b, h[1][4]),
        Err(Error::Capacity)
    );
    assert_eq!(o.channel_state().snapshot(), q);
    assert_eq!(o.stats(), stats);
}

#[test]
fn repeated_move_back_and_forth_reuses_handles_without_reviving_old_authority() {
    let (mut n, mut o, ids, h) = pair();
    for side in 0..2 {
        n.authority_mut(ids[side])
            .unwrap()
            .close(&mut o, ids[side], h[side][2])
            .unwrap();
    }
    let mut source = h[0][3];
    for round in 0..64 {
        let side = round % 2;
        let peer = 1 - side;
        n.authority_mut(ids[side])
            .unwrap()
            .send_cap(&mut o, ids[side], h[side][4], source)
            .unwrap();
        assert_eq!(read(&n, &mut o, ids[side], source), Err(Error::Stale));
        let received = n
            .authority_mut(ids[peer])
            .unwrap()
            .receive_cap(&mut o, ids[peer], h[peer][4])
            .unwrap();
        assert_eq!(read(&n, &mut o, ids[peer], received), Ok(1));
        source = received;
        conserved(&o);
    }
    for node in ids {
        n.authority_mut(node).unwrap().drain(&mut o, node).unwrap();
    }
    assert_eq!(o.stats(), Stats::default());
}

#[test]
fn eligible_source_survives_peer_close_without_queue_or_reference_changes() {
    let (mut n, mut o, [a, b], h) = pair();
    n.authority_mut(b).unwrap().drain(&mut o, b).unwrap();
    let q = o.channel_state().snapshot();
    let stats = o.stats();
    assert_eq!(
        n.authority_mut(a)
            .unwrap()
            .send_cap(&mut o, a, h[0][4], h[0][3]),
        Err(Error::PeerClosed)
    );
    assert_eq!(read(&n, &mut o, a, h[0][3]), Ok(1));
    assert_eq!(o.channel_state().snapshot(), q);
    assert_eq!(o.stats(), stats);
}

#[test]
fn queued_reference_and_existing_operation_pin_have_independent_lifetimes() {
    let (mut n, mut o, [a, b], h) = pair();
    let id = n.authority(a).unwrap().describe(a, h[0][3]).unwrap().0;
    o.pin(id).unwrap();
    n.authority_mut(a)
        .unwrap()
        .send_cap(&mut o, a, h[0][4], h[0][3])
        .unwrap();
    for node in [a, b] {
        n.authority_mut(node).unwrap().terminate();
        n.authority_mut(node).unwrap().drain(&mut o, node).unwrap();
    }
    assert_eq!(o.info(id).unwrap().references, 0);
    assert_eq!(o.info(id).unwrap().pins, 1);
    o.unpin(id);
    assert_eq!(o.info(id), Err(Error::Object));
    assert_eq!(o.stats(), Stats::default());
}

#[test]
fn sender_fault_before_commit_never_creates_queued_authority() {
    let (mut n, mut o, [a, b], h) = pair();
    let id = n.authority(a).unwrap().describe(a, h[0][3]).unwrap().0;
    n.authority_mut(a).unwrap().terminate();
    n.authority_mut(a).unwrap().drain(&mut o, a).unwrap();
    assert_eq!(o.info(id), Err(Error::Object));
    assert_eq!(o.channel_state().stats().queued, 0);
    assert_eq!(o.channel_state().transfer_snapshot(), [0; 12]);
    assert_eq!(read(&n, &mut o, b, h[1][3]), Ok(0));
    n.authority_mut(b).unwrap().drain(&mut o, b).unwrap();
    assert_eq!(o.stats(), Stats::default());
}

#[test]
fn movable_counter_keeps_write_only_rights_and_full_width_actual_value() {
    let (mut n, mut o, [a, b], h) = pair();
    let counter = o.counter(0).unwrap();
    let table = n.authority_mut(a).unwrap();
    table.sealed = false;
    table.close(&mut o, a, h[0][2]).unwrap();
    let source = table
        .grant_movable(&mut o, &counter, Rights::WRITE)
        .unwrap();
    table.seal_bootstrap();
    o.release(counter).unwrap();
    n.authority_mut(a)
        .unwrap()
        .send_cap(&mut o, a, h[0][4], source)
        .unwrap();
    n.authority_mut(b)
        .unwrap()
        .close(&mut o, b, h[1][2])
        .unwrap();
    let received = n
        .authority_mut(b)
        .unwrap()
        .receive_cap(&mut o, b, h[1][4])
        .unwrap();
    assert_eq!(
        n.authority(b)
            .unwrap()
            .lookup(&mut o, b, received, ObjectType::Counter, Rights::READ)
            .err(),
        Some(Error::Rights)
    );
    n.authority(b)
        .unwrap()
        .lookup(&mut o, b, received, ObjectType::Counter, Rights::WRITE)
        .unwrap()
        .write(u64::MAX)
        .unwrap();
    let id = n.authority(b).unwrap().describe(b, received).unwrap().0;
    assert!(matches!(
        o.object(id).unwrap().value,
        Value::Counter(u64::MAX)
    ));
    for node in [a, b] {
        n.authority_mut(node).unwrap().drain(&mut o, node).unwrap();
    }
    assert_eq!(o.stats(), Stats::default());
}

#[test]
fn move_counter_exhaustion_refuses_before_source_detach_or_recipient_install() {
    let (mut n, mut o, [a, b], h) = pair();
    o.channels.test_counters([u64::MAX, 0], [0; 2]);
    let q = o.channel_state().snapshot();
    let stats = o.stats();
    assert_eq!(
        n.authority_mut(a)
            .unwrap()
            .send_cap(&mut o, a, h[0][4], h[0][3]),
        Err(Error::Capacity)
    );
    assert_eq!(o.channel_state().snapshot(), q);
    assert_eq!(o.stats(), stats);
    assert_eq!(read(&n, &mut o, a, h[0][3]), Ok(1));
    o.channels.test_counters([0; 2], [0; 2]);
    n.authority_mut(a)
        .unwrap()
        .send_cap(&mut o, a, h[0][4], h[0][3])
        .unwrap();
    n.authority_mut(b)
        .unwrap()
        .close(&mut o, b, h[1][2])
        .unwrap();
    o.channels.test_counters([1, 0], [0, u64::MAX]);
    let q = o.channel_state().snapshot();
    let stats = o.stats();
    let gens = n
        .authority(b)
        .unwrap()
        .slots
        .each_ref()
        .map(|s| s.generation);
    assert_eq!(
        n.authority_mut(b).unwrap().receive_cap(&mut o, b, h[1][4]),
        Err(Error::Capacity)
    );
    assert_eq!(o.channel_state().snapshot(), q);
    assert_eq!(o.stats(), stats);
    assert_eq!(
        n.authority(b)
            .unwrap()
            .slots
            .each_ref()
            .map(|s| s.generation),
        gens
    );
    o.channels.test_counters([1, 0], [0; 2]);
    let received = n
        .authority_mut(b)
        .unwrap()
        .receive_cap(&mut o, b, h[1][4])
        .unwrap();
    assert_eq!(read(&n, &mut o, b, received), Ok(1));
    conserved(&o);
}

#[test]
fn actual_address_space_reference_cannot_move_or_lose_its_root_token() {
    let (mut n, mut o, [a, _], h) = pair();
    let mut registry = crate::paging::Registry::new();
    let space = registry.create_space().unwrap();
    registry.bind_root(space, 0x1000).unwrap();
    let creator = o
        .address_space(a, registry.retain_space(space).unwrap())
        .unwrap();
    n.authority_mut(a)
        .unwrap()
        .close(&mut o, a, h[0][2])
        .unwrap();
    let source = n
        .authority_mut(a)
        .unwrap()
        .grant(&mut o, &creator, Rights::INSPECT)
        .unwrap();
    o.release(creator).unwrap();
    let stats = o.stats();
    let q = o.channel_state().snapshot();
    assert_eq!(
        n.authority_mut(a)
            .unwrap()
            .send_cap(&mut o, a, h[0][4], source),
        Err(Error::Type)
    );
    assert_eq!(o.stats(), stats);
    assert_eq!(o.channel_state().snapshot(), q);
    assert_eq!(registry.object_references(space), Ok(1));
    n.authority_mut(a)
        .unwrap()
        .close(&mut o, a, source)
        .unwrap();
    registry.release_space_ref(o.reap_space().unwrap()).unwrap();
    assert_eq!(registry.object_references(space), Ok(0));
}

#[test]
fn two_queued_moves_preserve_order_and_release_two_distinct_references_to_one_object() {
    let (mut n, mut o, [a, b], h) = pair();
    let creator = o.counter(77).unwrap();
    let id = creator.id();
    let table = n.authority_mut(a).unwrap();
    table.sealed = false;
    for index in 0..2 {
        table.close(&mut o, a, h[0][index]).unwrap();
    }
    let first = table.grant_movable(&mut o, &creator, Rights::READ).unwrap();
    let second = table
        .grant_movable(&mut o, &creator, Rights::WRITE)
        .unwrap();
    table.seal_bootstrap();
    o.release(creator).unwrap();
    for source in [first, second] {
        n.authority_mut(a)
            .unwrap()
            .send_cap(&mut o, a, h[0][4], source)
            .unwrap();
        conserved(&o);
    }
    assert_eq!(o.info(id).unwrap().references, 2);
    assert_eq!(o.queued_references(), 2);
    for index in 0..2 {
        n.authority_mut(b)
            .unwrap()
            .close(&mut o, b, h[1][index])
            .unwrap();
    }
    let read_handle = n
        .authority_mut(b)
        .unwrap()
        .receive_cap(&mut o, b, h[1][4])
        .unwrap();
    let write_handle = n
        .authority_mut(b)
        .unwrap()
        .receive_cap(&mut o, b, h[1][4])
        .unwrap();
    assert_eq!(
        n.authority(b).unwrap().describe(b, read_handle),
        Ok((id, Rights::READ))
    );
    assert_eq!(
        n.authority(b).unwrap().describe(b, write_handle),
        Ok((id, Rights::WRITE))
    );
    assert_eq!(
        n.authority(b)
            .unwrap()
            .lookup(&mut o, b, read_handle, ObjectType::Counter, Rights::READ)
            .unwrap()
            .read(),
        Ok(77)
    );
    n.authority_mut(b)
        .unwrap()
        .close(&mut o, b, read_handle)
        .unwrap();
    assert_eq!(o.info(id).unwrap().references, 1);
    n.authority(b)
        .unwrap()
        .lookup(&mut o, b, write_handle, ObjectType::Counter, Rights::WRITE)
        .unwrap()
        .write(u64::MAX)
        .unwrap();
    n.authority_mut(b)
        .unwrap()
        .close(&mut o, b, write_handle)
        .unwrap();
    assert_eq!(o.info(id), Err(Error::Object));
    let replacement = o.counter(42).unwrap();
    assert_ne!(replacement.id(), id);
    o.release(replacement).unwrap();
    for node in [a, b] {
        n.authority_mut(node).unwrap().drain(&mut o, node).unwrap();
    }
    assert_eq!(o.stats(), Stats::default());
    conserved(&o);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelMessage {
    Scalar(u64),
    Capability(ObjectId, Rights),
}

#[derive(Clone, Copy)]
struct ModelTable {
    entries: [Option<(ObjectId, Rights, bool)>; HANDLES],
    generations: [u32; HANDLES],
    live: bool,
}

#[derive(Clone, Copy)]
struct ModelQueue {
    head: usize,
    tail: usize,
    len: usize,
    slots: [Option<ModelMessage>; crate::channel::MESSAGES],
}

impl ModelQueue {
    const EMPTY: Self = Self {
        head: 0,
        tail: 0,
        len: 0,
        slots: [None; crate::channel::MESSAGES],
    };

    fn push(&mut self, message: ModelMessage) {
        assert!(self.len < crate::channel::MESSAGES);
        assert!(self.slots[self.tail].is_none());
        self.slots[self.tail] = Some(message);
        self.tail = (self.tail + 1) % crate::channel::MESSAGES;
        self.len += 1;
    }

    fn pop(&mut self) -> ModelMessage {
        assert_ne!(self.len, 0);
        let message = self.slots[self.head].take().unwrap();
        self.head = (self.head + 1) % crate::channel::MESSAGES;
        self.len -= 1;
        message
    }

    fn words(self) -> [u64; 5] {
        let scalar = |slot| match self.slots[slot] {
            Some(ModelMessage::Scalar(value)) => value,
            _ => 0,
        };
        [
            self.head as u64,
            self.tail as u64,
            self.len as u64,
            scalar(0),
            scalar(1),
        ]
    }
}

#[derive(Clone, Copy)]
struct MoveModel {
    tables: [ModelTable; 2],
    queues: [ModelQueue; 2],
    sent: [u64; 2],
    received: [u64; 2],
    discarded: [u64; 2],
    live_ends: [bool; 2],
}

impl MoveModel {
    fn new(nodes: &Nodes, handles: [[Handle; 8]; 2], owners: [NodeId; 2]) -> (Self, [ObjectId; 4]) {
        let mut tables = [ModelTable {
            entries: [None; HANDLES],
            generations: [1; HANDLES],
            live: true,
        }; 2];
        for side in 0..2 {
            for slot in 0..HANDLES {
                let (object, rights) = nodes
                    .authority(owners[side])
                    .unwrap()
                    .describe(owners[side], handles[side][slot])
                    .unwrap();
                tables[side].entries[slot] = Some((object, rights, slot == 3));
            }
        }
        let objects = [
            nodes
                .authority(owners[0])
                .unwrap()
                .describe(owners[0], handles[0][0])
                .unwrap()
                .0,
            nodes
                .authority(owners[1])
                .unwrap()
                .describe(owners[1], handles[1][0])
                .unwrap()
                .0,
            nodes
                .authority(owners[0])
                .unwrap()
                .describe(owners[0], handles[0][4])
                .unwrap()
                .0,
            nodes
                .authority(owners[1])
                .unwrap()
                .describe(owners[1], handles[1][4])
                .unwrap()
                .0,
        ];
        (
            Self {
                tables,
                queues: [ModelQueue::EMPTY; 2],
                sent: [0; 2],
                received: [0; 2],
                discarded: [0; 2],
                live_ends: [true; 2],
            },
            objects,
        )
    }

    fn references(&self, object: ObjectId) -> u32 {
        let local = self
            .tables
            .iter()
            .flat_map(|table| table.entries.iter())
            .filter(|entry| entry.is_some_and(|(id, _, _)| id == object))
            .count();
        let queued = self
            .queues
            .iter()
            .flat_map(|queue| queue.slots.iter())
            .filter(
                |message| matches!(message, Some(ModelMessage::Capability(id, _)) if *id == object),
            )
            .count();
        (local + queued) as u32
    }

    fn snapshot(&self, owners: [NodeId; 2]) -> [u64; 22] {
        let mut words = [0; 22];
        words[..6].copy_from_slice(&[
            0x101,
            u64::from(self.live_ends.iter().any(|live| *live)),
            if self.live_ends[0] { 0x101 } else { 0 },
            if self.live_ends[1] { 0x102 } else { 0 },
            owners[0].raw(),
            owners[1].raw(),
        ]);
        words[6..11].copy_from_slice(&self.queues[0].words());
        words[11..16].copy_from_slice(&self.queues[1].words());
        words[16..22].copy_from_slice(&[
            self.sent[0],
            self.sent[1],
            self.received[0],
            self.received[1],
            self.discarded[0],
            self.discarded[1],
        ]);
        words
    }

    fn transfer_tags(&self) -> [u64; 12] {
        let mut words = [0; 12];
        for (side, queue) in self.queues.iter().enumerate() {
            for (slot, message) in queue.slots.iter().enumerate() {
                if let Some(ModelMessage::Capability(object, rights)) = message {
                    let offset = side * 6 + slot * 3;
                    words[offset..offset + 3].copy_from_slice(&[
                        1,
                        object.raw(),
                        rights.bits() as u64,
                    ]);
                }
            }
        }
        words
    }

    fn verify(&self, nodes: &Nodes, objects: &Objects, owners: [NodeId; 2], known: [ObjectId; 4]) {
        for side in 0..2 {
            if !self.tables[side].live {
                continue;
            }
            let table = nodes.authority(owners[side]).unwrap();
            for slot in 0..HANDLES {
                let handle = Handle::new(slot, self.tables[side].generations[slot]);
                match self.tables[side].entries[slot] {
                    Some((object, rights, _)) => {
                        assert_eq!(table.describe(owners[side], handle), Ok((object, rights)))
                    }
                    None => assert_eq!(table.describe(owners[side], handle), Err(Error::Stale)),
                }
            }
        }
        assert_eq!(objects.channel_state().snapshot(), self.snapshot(owners));
        assert_eq!(
            objects.channel_state().transfer_snapshot(),
            self.transfer_tags()
        );
        assert_eq!(
            objects.queued_references(),
            self.queues
                .iter()
                .flat_map(|q| q.slots.iter())
                .filter(|m| matches!(m, Some(ModelMessage::Capability(_, _))))
                .count() as u32
        );
        let references: u32 = known.iter().map(|object| self.references(*object)).sum();
        let live = known
            .iter()
            .filter(|object| self.references(**object) != 0)
            .count();
        assert_eq!(
            objects.stats(),
            Stats {
                live,
                references,
                pins: 0
            }
        );
        for object in known {
            let expected = self.references(object);
            if expected == 0 {
                assert_eq!(objects.info(object), Err(Error::Object));
            } else {
                assert_eq!(objects.info(object).unwrap().references, expected);
                assert_eq!(objects.info(object).unwrap().pins, 0);
            }
        }
        for side in 0..2 {
            assert_eq!(
                self.sent[side],
                self.received[1 - side]
                    + self.queues[1 - side].len as u64
                    + self.discarded[1 - side]
            );
        }
    }
}

#[derive(Clone, Copy)]
enum ModelOperation {
    SendScalar(usize, u64),
    SendCapability(usize),
    ReceiveScalar(usize),
    ReceiveCapability(usize),
    Close(usize, usize),
}

fn model_step(
    model: &mut MoveModel,
    nodes: &mut Nodes,
    objects: &mut Objects,
    owners: [NodeId; 2],
    known: [ObjectId; 4],
    operation: ModelOperation,
) {
    match operation {
        ModelOperation::SendScalar(side, value) => {
            let destination = 1 - side;
            let expected = if model.queues[destination].len == crate::channel::MESSAGES {
                Err(Error::Full)
            } else {
                Ok(())
            };
            let actual = nodes
                .authority(owners[side])
                .unwrap()
                .lookup(
                    objects,
                    owners[side],
                    Handle::new(4, 1),
                    ObjectType::ChannelEndpoint,
                    Rights::SEND,
                )
                .unwrap()
                .channel_send(value);
            assert_eq!(actual, expected);
            if actual.is_ok() {
                model.queues[destination].push(ModelMessage::Scalar(value));
                model.sent[side] += 1;
            }
        }
        ModelOperation::SendCapability(side) => {
            let slot = model.tables[side]
                .entries
                .iter()
                .position(|entry| entry.is_some_and(|(_, _, movable)| movable));
            let source = slot.map_or(Handle::new(0, model.tables[side].generations[0]), |slot| {
                Handle::new(slot, model.tables[side].generations[slot])
            });
            let destination = 1 - side;
            let expected = if slot.is_none() {
                if model.tables[side].entries[0].is_none() {
                    Err(Error::Stale)
                } else {
                    Err(Error::Transfer)
                }
            } else if model.queues[destination].len == crate::channel::MESSAGES {
                Err(Error::Full)
            } else {
                Ok(())
            };
            let actual = nodes.authority_mut(owners[side]).unwrap().send_cap(
                objects,
                owners[side],
                Handle::new(4, 1),
                source,
            );
            assert_eq!(actual, expected);
            if actual.is_ok() {
                let (object, rights, _) = model.tables[side].entries[slot.unwrap()].take().unwrap();
                model.queues[destination].push(ModelMessage::Capability(object, rights));
                model.sent[side] += 1;
            }
        }
        ModelOperation::ReceiveScalar(side) => {
            let head = if model.queues[side].len == 0 {
                None
            } else {
                model.queues[side].slots[model.queues[side].head]
            };
            let expected = match head {
                None => Err(Error::Empty),
                Some(ModelMessage::Capability(_, _)) => Err(Error::MessageKind),
                Some(ModelMessage::Scalar(value)) => Ok(value),
            };
            let actual = nodes
                .authority(owners[side])
                .unwrap()
                .lookup(
                    objects,
                    owners[side],
                    Handle::new(4, 1),
                    ObjectType::ChannelEndpoint,
                    Rights::RECEIVE,
                )
                .unwrap()
                .channel_receive();
            assert_eq!(actual, expected);
            if actual.is_ok() {
                assert_eq!(model.queues[side].pop(), head.unwrap());
                model.received[side] += 1;
            }
        }
        ModelOperation::ReceiveCapability(side) => {
            let head = if model.queues[side].len == 0 {
                None
            } else {
                model.queues[side].slots[model.queues[side].head]
            };
            let free = model.tables[side]
                .entries
                .iter()
                .enumerate()
                .find(|(slot, entry)| {
                    entry.is_none() && model.tables[side].generations[*slot] != u32::MAX
                })
                .map(|(slot, _)| slot);
            let expected = match head {
                None => Err(Error::Empty),
                Some(ModelMessage::Scalar(_)) => Err(Error::MessageKind),
                Some(ModelMessage::Capability(_, _)) if free.is_none() => Err(Error::Capacity),
                Some(ModelMessage::Capability(_, _)) => Ok(()),
            };
            let actual = nodes.authority_mut(owners[side]).unwrap().receive_cap(
                objects,
                owners[side],
                Handle::new(4, 1),
            );
            assert_eq!(actual.map(|_| ()), expected);
            if expected.is_ok() {
                let ModelMessage::Capability(object, rights) = model.queues[side].pop() else {
                    unreachable!()
                };
                let slot = free.unwrap();
                model.tables[side].generations[slot] += 1;
                model.tables[side].entries[slot] = Some((object, rights, true));
                model.received[side] += 1;
                assert_eq!(
                    actual.unwrap(),
                    Handle::new(slot, model.tables[side].generations[slot])
                );
            }
        }
        ModelOperation::Close(side, slot) => {
            let handle = Handle::new(slot, model.tables[side].generations[slot]);
            let expected = if model.tables[side].entries[slot].is_some() {
                Ok(())
            } else {
                Err(Error::Stale)
            };
            let actual =
                nodes
                    .authority_mut(owners[side])
                    .unwrap()
                    .close(objects, owners[side], handle);
            assert_eq!(actual, expected);
            if actual.is_ok() {
                model.tables[side].entries[slot] = None;
            }
        }
    }
    model.verify(nodes, objects, owners, known);
}

#[test]
fn seeded_mixed_fifo_sequences_match_an_independent_ownership_model_after_each_operation() {
    for seed in [1_u64, 7, 0x5eed, 0xc0ffee] {
        let (mut nodes, mut objects, owners, handles) = pair();
        let (mut model, known) = MoveModel::new(&nodes, handles, owners);

        // Edge-focused prefix: scalar-before-capability, wrong-kind rollback,
        // both directions, ring wraparound, and full rejection/retry.
        model_step(
            &mut model,
            &mut nodes,
            &mut objects,
            owners,
            known,
            ModelOperation::Close(1, 2),
        );
        for operation in [
            ModelOperation::SendScalar(0, 0x10),
            ModelOperation::SendCapability(0),
            ModelOperation::ReceiveCapability(1),
            ModelOperation::ReceiveScalar(1),
            ModelOperation::ReceiveCapability(1),
            ModelOperation::SendScalar(1, 0x20),
            ModelOperation::SendCapability(1),
            ModelOperation::ReceiveCapability(0),
            ModelOperation::ReceiveScalar(0),
            ModelOperation::ReceiveCapability(0),
            ModelOperation::SendScalar(0, 0x30),
            ModelOperation::SendCapability(0),
            ModelOperation::SendScalar(0, 0x31),
            ModelOperation::ReceiveCapability(1),
            ModelOperation::ReceiveScalar(1),
            ModelOperation::SendScalar(1, 0x40),
            ModelOperation::SendScalar(1, 0x41),
            ModelOperation::ReceiveScalar(0),
            ModelOperation::ReceiveScalar(0),
        ] {
            model_step(
                &mut model,
                &mut nodes,
                &mut objects,
                owners,
                known,
                operation,
            );
        }

        let mut random = seed;
        for index in 0..96 {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            let side = ((random >> 8) & 1) as usize;
            let operation = match random % 5 {
                0 => {
                    ModelOperation::SendScalar(side, random.rotate_left(index % 63) ^ index as u64)
                }
                1 => ModelOperation::ReceiveScalar(side),
                2 => ModelOperation::SendCapability(side),
                3 => ModelOperation::ReceiveCapability(side),
                _ => ModelOperation::Close(side, ((random >> 16) as usize) % 4),
            };
            model_step(
                &mut model,
                &mut nodes,
                &mut objects,
                owners,
                known,
                operation,
            );
        }

        // Node teardown is modeled as endpoint retirement plus table drainage.
        for side in 0..2 {
            nodes.authority_mut(owners[side]).unwrap().terminate();
            nodes
                .authority_mut(owners[side])
                .unwrap()
                .drain(&mut objects, owners[side])
                .unwrap();
            model.discarded[side] += model.queues[side].len as u64;
            model.queues[side] = ModelQueue::EMPTY;
            model.live_ends[side] = false;
            model.tables[side].entries = [None; HANDLES];
            model.tables[side].live = false;
            model.verify(&nodes, &objects, owners, known);
        }
        assert_eq!(objects.stats(), Stats::default());
        assert_eq!(
            objects.channel_state().stats(),
            crate::channel::Stats::default()
        );
    }
}

/// K8B shape: Node A's manifest index 3 is a MemoryObject capability carrying
/// TRANSFER; Node B keeps four Flags so a full receive table is reachable.
fn memory_pair() -> (
    Nodes,
    Objects,
    crate::paging::Registry,
    [NodeId; 2],
    [[Handle; 8]; 2],
    crate::paging::MemoryObjectId,
) {
    let mut n = Nodes::new();
    let owners = [n.create().unwrap(), n.create().unwrap()];
    let mut o = Objects::with_channels();
    let mut r = crate::paging::Registry::new();
    let frames = [r.insert(0x200000).unwrap(), r.insert(0x201000).unwrap()];
    for frame in frames {
        r.initialize(frame).unwrap();
    }
    let object = r.create_memory_object(&frames).unwrap();
    let reference = r.retain_memory_object(object).unwrap();
    let capability = o.memory_object(reference).unwrap();
    let flag = o.flag(true).unwrap();
    for index in 0..3 {
        n.authority_mut(owners[0])
            .unwrap()
            .install_bootstrap(&mut o, &flag, Rights::READ, index)
            .unwrap();
    }
    n.authority_mut(owners[0])
        .unwrap()
        .install_bootstrap(&mut o, &capability, Rights::MEMORY_OBJECT, 3)
        .unwrap();
    o.release(flag).unwrap();
    o.release(capability).unwrap();
    r.release_memory_object(object).unwrap();
    let flag = o.flag(false).unwrap();
    for index in 0..4 {
        n.authority_mut(owners[1])
            .unwrap()
            .install_bootstrap(&mut o, &flag, Rights::READ, index)
            .unwrap();
    }
    o.release(flag).unwrap();
    n.bootstrap_channel(&mut o, owners[0], owners[1]).unwrap();
    n.authority_mut(owners[0])
        .unwrap()
        .configure_memory_object_moves()
        .unwrap();
    n.authority_mut(owners[1])
        .unwrap()
        .configure_moves()
        .unwrap();
    for owner in owners {
        n.authority_mut(owner).unwrap().seal_bootstrap();
    }
    let handles = owners.map(|owner| {
        core::array::from_fn(|index| {
            n.authority(owner)
                .unwrap()
                .bootstrap_get(&o, owner, index as u64)
                .unwrap()
        })
    });
    (n, o, r, owners, handles, object)
}

#[test]
fn memory_object_capability_moves_once_with_exact_rights_and_one_paging_reference() {
    let (mut n, mut o, mut r, [a, b], h, object) = memory_pair();
    let (id, rights) = n.authority(a).unwrap().describe(a, h[0][3]).unwrap();
    assert_eq!(id.kind(), ObjectType::MemoryObject);
    assert_eq!(rights, Rights::MEMORY_OBJECT);
    assert_eq!(r.memory_object_capability_refs(object), Ok(1));
    assert_eq!(r.memory_object_counts(object), Ok((0, 0, 2)));

    let before = o.stats();
    n.authority_mut(a)
        .unwrap()
        .send_cap(&mut o, a, h[0][4], h[0][3])
        .unwrap();
    assert_eq!(
        n.authority(a).unwrap().describe(a, h[0][3]),
        Err(Error::Stale)
    );
    assert_eq!(
        n.authority_mut(a).unwrap().close(&mut o, a, h[0][3]),
        Err(Error::Stale)
    );
    // The moved reference now lives in the FIFO; totals never change.
    assert_eq!(o.stats(), before);
    assert_eq!(o.queued_references(), 1);
    assert_eq!(o.channel_state().stats().queued, 1);
    assert_eq!(r.memory_object_capability_refs(object), Ok(1));

    // A full receiver table rejects delivery without touching queued authority.
    let queue = o.channel_state().snapshot();
    assert_eq!(
        n.authority_mut(b).unwrap().receive_cap(&mut o, b, h[1][4]),
        Err(Error::Capacity)
    );
    assert_eq!(o.channel_state().snapshot(), queue);
    assert_eq!(o.stats(), before);
    assert_eq!(r.memory_object_capability_refs(object), Ok(1));

    n.authority_mut(b)
        .unwrap()
        .close(&mut o, b, h[1][2])
        .unwrap();
    let received = n
        .authority_mut(b)
        .unwrap()
        .receive_cap(&mut o, b, h[1][4])
        .unwrap();
    assert_ne!(received, h[0][3]);
    assert_eq!(
        n.authority(b).unwrap().describe(b, received),
        Ok((id, Rights::MEMORY_OBJECT))
    );
    {
        let guard = n
            .authority(b)
            .unwrap()
            .lookup(
                &mut o,
                b,
                received,
                ObjectType::MemoryObject,
                Rights::READ_WRITE,
            )
            .unwrap();
        assert_eq!(guard.memory_object(true).map(|r| r.id()), Ok(object));
        assert_eq!(guard.memory_object(false).map(|r| r.id()), Ok(object));
        assert_eq!(guard.read(), Err(Error::Type));
    }
    assert_eq!(o.queued_references(), 0);
    assert_eq!(r.memory_object_capability_refs(object), Ok(1));
    assert_eq!(r.memory_object_counts(object), Ok((0, 0, 2)));

    // Closing the last capability handle parks exactly one paging reference.
    n.authority_mut(b)
        .unwrap()
        .close(&mut o, b, received)
        .unwrap();
    assert_eq!(o.pending_memory_objects(), 1);
    assert_eq!(r.memory_object_capability_refs(object), Ok(1));
    assert_eq!(
        r.collect_memory_object(object),
        Err(crate::paging::Error::InUse)
    );
    let reference = o.drain_memory_object().unwrap();
    assert_eq!(reference.id(), object);
    assert_eq!(o.pending_memory_objects(), 0);
    r.release_memory_object_ref(reference).unwrap();
    assert_eq!(r.memory_object_capability_refs(object), Ok(0));
    let frames = r.collect_memory_object(object).unwrap();
    assert_eq!(frames.iter().flatten().count(), 2);
    assert_eq!(
        r.collect_memory_object(object),
        Err(crate::paging::Error::NotOwned)
    );
    for frame in frames.into_iter().flatten() {
        r.retire(frame).unwrap();
    }
}

#[test]
fn memory_object_send_requires_transfer_and_failures_preserve_source_and_queue() {
    let (mut n, mut o, mut r, [a, b], h, object) = memory_pair();
    let stats = o.stats();
    // The capability is movable but carries no TRANSFER right.
    n.authority_mut(a).unwrap().slots[3].rights = Rights::MAP;
    assert_eq!(
        n.authority_mut(a)
            .unwrap()
            .send_cap(&mut o, a, h[0][4], h[0][3]),
        Err(Error::Rights)
    );
    assert_eq!(o.queued_references(), 0);
    assert_eq!(o.stats(), stats);
    assert_eq!(r.memory_object_capability_refs(object), Ok(1));
    n.authority_mut(a).unwrap().slots[3].rights = Rights::MEMORY_OBJECT;

    // A closed peer refuses the send before detachment.
    n.authority_mut(b).unwrap().terminate();
    n.authority_mut(b).unwrap().drain(&mut o, b).unwrap();
    let drained = o.stats();
    assert_eq!(
        n.authority_mut(a)
            .unwrap()
            .send_cap(&mut o, a, h[0][4], h[0][3]),
        Err(Error::PeerClosed)
    );
    assert_eq!(o.stats(), drained);
    assert_eq!(o.queued_references(), 0);
    assert_eq!(r.memory_object_capability_refs(object), Ok(1));
    assert_eq!(
        n.authority(a)
            .unwrap()
            .describe(a, h[0][3])
            .unwrap()
            .0
            .kind(),
        ObjectType::MemoryObject
    );

    // Sender teardown still parks exactly one reference for the architecture.
    n.authority_mut(a).unwrap().terminate();
    n.authority_mut(a).unwrap().drain(&mut o, a).unwrap();
    assert_eq!(o.pending_memory_objects(), 1);
    assert_eq!(o.stats(), Stats::default());
    assert_eq!(r.memory_object_capability_refs(object), Ok(1));
    let reference = o.drain_memory_object().unwrap();
    r.release_memory_object_ref(reference).unwrap();
    let frames = r.collect_memory_object(object).unwrap();
    assert_eq!(frames.iter().flatten().count(), 2);
    for frame in frames.into_iter().flatten() {
        r.retire(frame).unwrap();
    }
}

#[test]
fn queued_memory_object_capability_is_discarded_exactly_once_across_teardown() {
    for received in [false, true] {
        let (mut n, mut o, mut r, [a, b], h, object) = memory_pair();
        n.authority_mut(a)
            .unwrap()
            .send_cap(&mut o, a, h[0][4], h[0][3])
            .unwrap();
        n.authority_mut(a).unwrap().terminate();
        n.authority_mut(a).unwrap().drain(&mut o, a).unwrap();
        // The queued reference outlives its sender and keeps the paging
        // capability reference alive.
        assert_eq!(o.queued_references(), 1);
        assert_eq!(o.pending_memory_objects(), 0);
        assert_eq!(r.memory_object_capability_refs(object), Ok(1));
        if received {
            n.authority_mut(b)
                .unwrap()
                .close(&mut o, b, h[1][2])
                .unwrap();
            let handle = n
                .authority_mut(b)
                .unwrap()
                .receive_cap(&mut o, b, h[1][4])
                .unwrap();
            assert_eq!(
                n.authority(b).unwrap().describe(b, handle).unwrap().1,
                Rights::MEMORY_OBJECT
            );
            assert_eq!(o.queued_references(), 0);
        }
        n.authority_mut(b).unwrap().terminate();
        n.authority_mut(b).unwrap().drain(&mut o, b).unwrap();
        assert_eq!(o.queued_references(), 0);
        assert_eq!(o.channel_state().stats(), crate::channel::Stats::default());
        assert_eq!(o.stats(), Stats::default());
        assert_eq!(o.pending_memory_objects(), 1);
        assert_eq!(r.memory_object_capability_refs(object), Ok(1));
        assert_eq!(
            r.collect_memory_object(object),
            Err(crate::paging::Error::InUse)
        );
        let reference = o.drain_memory_object().unwrap();
        assert_eq!(reference.id(), object);
        assert_eq!(o.drain_memory_object().map(|r| r.id()), None);
        r.release_memory_object_ref(reference).unwrap();
        let frames = r.collect_memory_object(object).unwrap();
        assert_eq!(frames.iter().flatten().count(), 2);
        assert_eq!(
            r.collect_memory_object(object),
            Err(crate::paging::Error::NotOwned)
        );
        for frame in frames.into_iter().flatten() {
            r.retire(frame).unwrap();
            assert_eq!(r.retire(frame), Err(crate::paging::Error::NotOwned));
        }
    }
}
