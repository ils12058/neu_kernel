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
        Some(Value::Counter(u64::MAX))
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
