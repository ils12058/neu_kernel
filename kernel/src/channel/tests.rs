use super::*;
fn owners() -> [NodeId; 2] {
    let mut n = crate::node::Nodes::new();
    [n.create().unwrap(), n.create().unwrap()]
}
#[test]
fn duplex_fifo_preserves_zero_full_width_and_independent_order() {
    let mut c = Channels::new();
    let [a, b] = owners();
    let [ea, eb] = c.pair([a, b]).unwrap();
    for v in [0, u64::MAX] {
        c.send(ea.id(), a, v).unwrap();
    }
    for v in [0xfedcba9876543210, 99] {
        c.send(eb.id(), b, v).unwrap();
    }
    assert_eq!(c.receive(eb.id(), b), Ok(0));
    assert_eq!(c.receive(ea.id(), a), Ok(0xfedcba9876543210));
    assert_eq!(c.receive(eb.id(), b), Ok(u64::MAX));
    assert_eq!(c.receive(ea.id(), a), Ok(99));
}
#[test]
fn full_and_empty_failures_do_not_mutate_any_queue_or_counter() {
    let mut c = Channels::new();
    let [a, b] = owners();
    let [ea, eb] = c.pair([a, b]).unwrap();
    let before = c.snapshot();
    assert_eq!(c.receive(ea.id(), a), Err(Error::Empty));
    assert_eq!(c.snapshot(), before);
    c.send(ea.id(), a, 7).unwrap();
    c.send(ea.id(), a, 8).unwrap();
    let full = c.snapshot();
    assert_eq!(c.send(ea.id(), a, 9), Err(Error::Full));
    assert_eq!(c.snapshot(), full);
    assert_eq!(c.receive(eb.id(), b), Ok(7));
    c.send(ea.id(), a, 9).unwrap();
    assert_eq!(c.receive(eb.id(), b), Ok(8));
    assert_eq!(c.receive(eb.id(), b), Ok(9));
}
#[test]
fn peer_close_preserves_already_committed_inbound_and_reports_closed_after_drain() {
    let mut c = Channels::new();
    let [a, b] = owners();
    let [ea, eb] = c.pair([a, b]).unwrap();
    c.send(ea.id(), a, 0).unwrap();
    c.send(ea.id(), a, 42).unwrap();
    c.retire(ea).unwrap();
    assert_eq!(c.send(eb.id(), b, 1), Err(Error::PeerClosed));
    assert_eq!(c.receive(eb.id(), b), Ok(0));
    assert_eq!(c.receive(eb.id(), b), Ok(42));
    assert_eq!(c.receive(eb.id(), b), Err(Error::PeerClosed));
    c.retire(eb).unwrap();
    assert_eq!(c.stats(), Stats::default());
}
#[test]
fn retiring_receiver_discards_only_undeliverable_direction() {
    let mut c = Channels::new();
    let [a, b] = owners();
    let [ea, eb] = c.pair([a, b]).unwrap();
    c.send(ea.id(), a, 11).unwrap();
    c.send(eb.id(), b, 22).unwrap();
    c.retire(ea).unwrap();
    assert_eq!(c.snapshot()[20..22], [1, 0]);
    assert_eq!(c.stats().queued, 1);
    assert_eq!(c.receive(eb.id(), b), Ok(11));
    c.retire(eb).unwrap();
    assert_eq!(c.stats(), Stats::default());
}
#[test]
fn channel_and_endpoint_generations_reject_recycled_ids() {
    let mut c = Channels::new();
    let [a, b] = owners();
    let [ea, eb] = c.pair([a, b]).unwrap();
    let old = ea.id();
    let chid = c.info(old).unwrap().channel;
    c.retire(ea).unwrap();
    c.retire(eb).unwrap();
    let [ea, eb] = c.pair([a, b]).unwrap();
    assert_ne!(ea.id(), old);
    assert_ne!(c.info(ea.id()).unwrap().channel, chid);
    let before = c.snapshot();
    assert_eq!(c.send(old, a, 3), Err(Error::Object));
    assert_eq!(c.snapshot(), before);
    assert_eq!(c.receive(eb.id(), b), Err(Error::Empty));
}
#[test]
fn wrong_owner_and_duplicate_owner_cannot_publish_or_mutate() {
    let mut c = Channels::new();
    let [a, b] = owners();
    let before = c.snapshot();
    assert!(matches!(c.pair([a, a]), Err(Error::Owner)));
    assert_eq!(c.snapshot(), before);
    let [ea, _] = c.pair([a, b]).unwrap();
    let before = c.snapshot();
    assert_eq!(c.send(ea.id(), b, 1), Err(Error::Owner));
    assert_eq!(c.receive(ea.id(), b), Err(Error::Owner));
    assert_eq!(c.snapshot(), before);
}
#[test]
fn channel_pool_and_endpoint_generation_exhaustion_are_atomic() {
    let mut c = Channels::new();
    let [a, b] = owners();
    let _pair = c.pair([a, b]).unwrap();
    let before = c.snapshot();
    assert!(matches!(c.pair([a, b]), Err(Error::Capacity)));
    assert_eq!(c.snapshot(), before);
    for endpoint in [false, true] {
        let mut c = Channels::new();
        if endpoint {
            c.endpoints[1].generation = u32::MAX;
        } else {
            c.generation = u32::MAX;
        }
        let before = c.snapshot();
        assert!(matches!(c.pair([a, b]), Err(Error::Capacity)));
        assert_eq!(c.snapshot(), before);
    }
}
#[test]
fn accounting_counter_exhaustion_precedes_message_commit() {
    let mut c = Channels::new();
    let [a, b] = owners();
    let [ea, eb] = c.pair([a, b]).unwrap();
    c.sent[0] = u64::MAX;
    let before = c.snapshot();
    assert_eq!(c.send(ea.id(), a, 5), Err(Error::Capacity));
    assert_eq!(c.snapshot(), before);
    c.sent[0] = 0;
    c.send(ea.id(), a, 5).unwrap();
    c.received[1] = u64::MAX;
    let before = c.snapshot();
    assert_eq!(c.receive(eb.id(), b), Err(Error::Capacity));
    assert_eq!(c.snapshot(), before);
}
#[test]
fn stale_peer_generation_is_rejected_before_queue_mutation() {
    let mut c = Channels::new();
    let [a, b] = owners();
    let [ea, _eb] = c.pair([a, b]).unwrap();
    c.endpoints[1].generation += 1;
    let before = c.snapshot();
    assert_eq!(c.send(ea.id(), a, 99), Err(Error::Object));
    assert_eq!(c.receive(ea.id(), a), Err(Error::Object));
    assert_eq!(c.snapshot(), before);
}
#[test]
fn terminal_channel_identity_rejects_operations_without_mutation() {
    let mut c = Channels::new();
    let [a, b] = owners();
    let [ea, _eb] = c.pair([a, b]).unwrap();
    c.live = false;
    let before = c.snapshot();
    assert_eq!(c.send(ea.id(), a, 99), Err(Error::Terminal));
    assert_eq!(c.receive(ea.id(), a), Err(Error::Terminal));
    assert_eq!(c.snapshot(), before);
}
