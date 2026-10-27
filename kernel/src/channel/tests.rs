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

fn assert_invariants(
    c: &Channels,
    queues: &[std::collections::VecDeque<u64>; 2],
    sent: [u64; 2],
    received: [u64; 2],
    discarded: [u64; 2],
) {
    let w = c.snapshot();
    for receiver in 0..2 {
        let base = 6 + receiver * 5;
        let (head, tail, len) = (w[base], w[base + 1], w[base + 2]);
        assert!(head < 2 && tail < 2 && len <= 2);
        assert_eq!((head + len) % 2, tail);
        assert_eq!(len as usize, queues[receiver].len());
        for (offset, value) in queues[receiver].iter().enumerate() {
            assert_eq!(w[base + 3 + (head as usize + offset) % 2], *value);
        }
        assert_eq!(w[16 + receiver], sent[receiver]);
        assert_eq!(w[18 + receiver], received[receiver]);
        assert_eq!(w[20 + receiver], discarded[receiver]);
        assert_eq!(
            sent[1 - receiver],
            received[receiver] + len + discarded[receiver]
        );
    }
}

#[test]
fn seeded_duplex_state_machine_conserves_messages_across_close_and_reuse() {
    // Deterministic xorshift seeds; VecDeque is an independent logical FIFO,
    // with no knowledge of the production ring indices or slot placement.
    for seed in 1_u64..=128 {
        let mut random = seed;
        let mut c = Channels::new();
        let nodes = owners();
        let mut old = None;
        for _generation in 0..8 {
            let [a, b] = c.pair(nodes).unwrap();
            let mut ends = [Some(a), Some(b)];
            let ids = [
                ends[0].as_ref().unwrap().id(),
                ends[1].as_ref().unwrap().id(),
            ];
            if let Some(stale) = old {
                assert_eq!(c.send(stale, nodes[0], 1), Err(Error::Object));
            }
            let mut queues = [
                std::collections::VecDeque::new(),
                std::collections::VecDeque::new(),
            ];
            let (mut sent, mut received, mut discarded) = ([0; 2], [0; 2], [0; 2]);
            for _ in 0..120 {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                let side = (random as usize) & 1;
                let peer = 1 - side;
                let before = c.snapshot();
                match (random >> 1) % 10 {
                    0..=4 if ends[side].is_some() => {
                        let payload = match (random >> 8) & 3 {
                            0 => 0,
                            1 => u64::MAX,
                            _ => random,
                        };
                        let expected = if ends[peer].is_none() {
                            Err(Error::PeerClosed)
                        } else if queues[peer].len() == MESSAGES {
                            Err(Error::Full)
                        } else {
                            queues[peer].push_back(payload);
                            sent[side] += 1;
                            Ok(())
                        };
                        assert_eq!(
                            c.send(ids[side], nodes[side], payload),
                            expected,
                            "seed {seed}"
                        );
                        if expected.is_err() {
                            assert_eq!(c.snapshot(), before);
                        }
                    }
                    5..=7 if ends[side].is_some() => {
                        let expected = if let Some(value) = queues[side].pop_front() {
                            received[side] += 1;
                            Ok(value)
                        } else if ends[peer].is_some() {
                            Err(Error::Empty)
                        } else {
                            Err(Error::PeerClosed)
                        };
                        assert_eq!(c.receive(ids[side], nodes[side]), expected, "seed {seed}");
                        if expected.is_err() {
                            assert_eq!(c.snapshot(), before);
                        }
                    }
                    8 if ends[side].is_some() => {
                        discarded[side] += queues[side].len() as u64;
                        queues[side].clear();
                        c.retire(ends[side].take().unwrap()).unwrap();
                    }
                    _ => {
                        assert!(c.send(ids[side], nodes[peer], random).is_err());
                        assert_eq!(c.snapshot(), before);
                    }
                }
                assert_invariants(&c, &queues, sent, received, discarded);
            }
            for side in 0..2 {
                if let Some(endpoint) = ends[side].take() {
                    discarded[side] += queues[side].len() as u64;
                    queues[side].clear();
                    c.retire(endpoint).unwrap();
                    assert_invariants(&c, &queues, sent, received, discarded);
                }
            }
            assert_eq!(c.stats(), Stats::default());
            old = Some(ids[0]);
        }
    }
}

#[test]
fn repeated_bidirectional_backpressure_wraps_without_changing_rejected_state() {
    let mut c = Channels::new();
    let nodes = owners();
    let [a, b] = c.pair(nodes).unwrap();
    let ids = [a.id(), b.id()];
    for round in 0..64_u64 {
        for side in 0..2 {
            c.send(ids[side], nodes[side], round * 8 + side as u64)
                .unwrap();
            c.send(ids[side], nodes[side], u64::MAX - round).unwrap();
        }
        for side in 0..2 {
            let before = c.snapshot();
            assert_eq!(c.send(ids[side], nodes[side], 1), Err(Error::Full));
            assert_eq!(c.snapshot(), before);
        }
        for side in 0..2 {
            assert_eq!(
                c.receive(ids[side], nodes[side]),
                Ok(round * 8 + (1 - side) as u64)
            );
            c.send(ids[1 - side], nodes[1 - side], round).unwrap();
            assert_eq!(c.receive(ids[side], nodes[side]), Ok(u64::MAX - round));
            assert_eq!(c.receive(ids[side], nodes[side]), Ok(round));
            let empty = c.snapshot();
            assert_eq!(c.receive(ids[side], nodes[side]), Err(Error::Empty));
            assert_eq!(c.snapshot(), empty);
        }
    }
}

#[test]
fn corrupt_fifo_and_discard_overflow_refuse_before_retirement_mutates() {
    for corruption in 0..4 {
        let mut c = Channels::new();
        let [a, b] = owners();
        let [ea, eb] = c.pair([a, b]).unwrap();
        c.send(ea.id(), a, 12).unwrap();
        match corruption {
            0 => c.queues[1].head = 2,
            1 => c.queues[1].tail = 2,
            2 => c.queues[1].len = 3,
            _ => c.queues[1].tail = 0,
        }
        let before = c.snapshot();
        assert_eq!(c.send(ea.id(), a, 1), Err(Error::Terminal));
        assert_eq!(c.receive(eb.id(), b), Err(Error::Terminal));
        assert_eq!(c.retire(eb), Err(Error::Terminal));
        assert_eq!(c.snapshot(), before);
    }
    let mut c = Channels::new();
    let [a, b] = owners();
    let [ea, eb] = c.pair([a, b]).unwrap();
    c.send(ea.id(), a, 12).unwrap();
    c.discarded[1] = u64::MAX;
    let before = c.snapshot();
    assert_eq!(c.retire(eb), Err(Error::Terminal));
    assert_eq!(c.snapshot(), before);
}

#[test]
fn recycled_endpoint_generations_cannot_satisfy_a_stale_pending_identity() {
    let mut c = Channels::new();
    let [a, b] = owners();
    let [ea, eb] = c.pair([a, b]).unwrap();
    let stale = ea.id();
    let stale_channel = c.info(stale).unwrap().channel;
    c.retire(ea).unwrap();
    c.retire(eb).unwrap();
    let [ea, eb] = c.pair([a, b]).unwrap();
    assert_ne!(ea.id(), stale);
    assert_ne!(c.info(ea.id()).unwrap().channel, stale_channel);
    assert_eq!(c.info(stale), Err(Error::Object));
    assert_eq!(c.receive(stale, a), Err(Error::Object));
    assert_eq!(c.send(stale, a, 1), Err(Error::Object));
    assert_eq!(c.receive(eb.id(), b), Err(Error::Empty));
}
