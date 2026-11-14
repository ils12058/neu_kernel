use super::*;
use crate::node::Nodes;

#[test]
fn typed_device_rights_cannot_amplify_or_cross_a_node_boundary() {
    let mut n = Nodes::new();
    let a = n.create().unwrap();
    let b = n.create().unwrap();
    let mut o = Objects::with_devices();
    let (m, i) = o
        .install_device_lease(n.authority_mut(a).unwrap(), 7)
        .unwrap();
    let table = n.authority_mut(a).unwrap();
    assert_eq!(
        table
            .lookup(&mut o, a, m, ObjectType::MmioRegion, Rights::READ)
            .unwrap()
            .device_generation(),
        Ok(7)
    );
    assert!(matches!(
        table.lookup(&mut o, a, m, ObjectType::Interrupt, Rights::WAIT),
        Err(Error::Type)
    ));
    assert!(matches!(
        table.lookup(&mut o, b, m, ObjectType::MmioRegion, Rights::READ),
        Err(Error::Owner)
    ));
    let read = table.derive(&mut o, a, m, Rights::READ).unwrap();
    assert!(matches!(
        table.lookup(&mut o, a, read, ObjectType::MmioRegion, Rights::WRITE),
        Err(Error::Rights)
    ));
    assert_eq!(table.derive(&mut o, a, m, Rights::MAP), Err(Error::Rights));
    assert_eq!(
        table.derive(&mut o, a, i, Rights::TRANSFER),
        Err(Error::Rights)
    );
    assert_eq!(table.retire_device_lease(&mut o, 6), Ok(0));
    assert_eq!(table.retire_device_lease(&mut o, 7), Ok(3));
    assert_eq!(o.stats(), Stats::default());
    assert_eq!(o.live_grants(), 0);
    assert!(matches!(
        table.lookup(&mut o, a, read, ObjectType::MmioRegion, Rights::READ),
        Err(Error::Stale)
    ));
}

#[test]
fn device_pair_capacity_failure_leaves_no_partial_authority() {
    let mut n = Nodes::new();
    let a = n.create().unwrap();
    let table = n.authority_mut(a).unwrap();
    let mut o = Objects::with_devices();
    let resource = o.device_resource().unwrap();
    for _ in 0..7 {
        table.grant(&mut o, &resource, Rights::INSPECT).unwrap();
    }
    let before = o.stats();
    assert_eq!(o.install_device_lease(table, 1), Err(Error::Capacity));
    assert_eq!(o.stats(), before);
    assert_eq!(table.len(), 7);
    table.drain(&mut o, a).unwrap();
    o.release(resource).unwrap();
    assert_eq!(o.stats(), Stats::default());
}

#[test]
fn second_lineage_allocation_failure_rolls_back_both_device_objects() {
    let mut n = Nodes::new();
    let a = n.create().unwrap();
    let mut o = Objects::with_devices();
    let obj = o.device_resource().unwrap();
    let rights = Rights::INSPECT.union(Rights::DERIVE);
    // Occupy lineage directly as a trusted test fixture, leaving exactly one
    // slot: failure must occur after the provisional first lease handle.
    let mut grants = std::vec::Vec::new();
    for _ in 0..GRANTS - 1 {
        grants.push(o.lineage.root(obj.id, rights).unwrap());
    }
    let before = o.stats();
    let live = o.live_grants();
    assert_eq!(
        o.install_device_lease(n.authority_mut(a).unwrap(), 1),
        Err(Error::Capacity)
    );
    assert!(n.authority(a).unwrap().is_empty());
    assert_eq!(o.stats(), before);
    assert_eq!(o.live_grants(), live);
    for id in grants {
        o.lineage.release(id, Use::Handle).unwrap();
    }
    o.release(obj).unwrap();
    assert_eq!(o.stats(), Stats::default());
}

#[test]
fn strict_descendant_revoke_keeps_source_and_can_select_a_device_wait_grant() {
    let mut n = Nodes::new();
    let a = n.create().unwrap();
    let mut o = Objects::with_devices();
    let table = n.authority_mut(a).unwrap();
    let (m, i) = o.install_device_lease(table, 1).unwrap();
    let child = table.derive(&mut o, a, i, Rights::WAIT).unwrap();
    let source = table.grant_id(&o, a, i).unwrap();
    let waiter = table.grant_id(&o, a, child).unwrap();
    let ticket = table.begin_revoke(&mut o, a, i).unwrap();
    assert_eq!(o.revoke_selects(&ticket, source), Ok(false));
    assert_eq!(o.revoke_selects(&ticket, waiter), Ok(true));
    assert_eq!(table.purge_revoked(&mut o), Ok(1));
    assert_eq!(o.finish_revoke(&ticket), Ok(1));
    assert!(
        table
            .lookup(&mut o, a, i, ObjectType::Interrupt, Rights::WAIT)
            .is_ok()
    );
    table.close(&mut o, a, m).unwrap();
    table.close(&mut o, a, i).unwrap();
    assert_eq!(o.stats(), Stats::default());
    assert_eq!(o.live_grants(), 0);
}

#[test]
fn dma_types_attenuate_without_transfer_or_foreign_node_authority() {
    let mut nodes = Nodes::new();
    let a = nodes.create().unwrap();
    let b = nodes.create().unwrap();
    let mut objects = Objects::with_devices();
    let table = nodes.authority_mut(a).unwrap();
    let domain = objects
        .install_dma_capability(table, ObjectType::DmaDomain, 9)
        .unwrap();
    let buffer = objects
        .install_dma_capability(table, ObjectType::DmaBuffer, 9)
        .unwrap();
    for (handle, kind, rights) in [
        (domain, ObjectType::DmaDomain, Rights::SEND),
        (buffer, ObjectType::DmaBuffer, Rights::READ_WRITE),
    ] {
        assert_eq!(
            table
                .lookup(&mut objects, a, handle, kind, rights)
                .unwrap()
                .device_generation(),
            Ok(9)
        );
        assert!(matches!(
            table.lookup(&mut objects, b, handle, kind, rights),
            Err(Error::Owner)
        ));
        assert_eq!(
            table.derive(&mut objects, a, handle, Rights::TRANSFER),
            Err(Error::Rights)
        );
        assert!(!table.entry(a, handle).unwrap().transferable);
    }
    let readonly = table.derive(&mut objects, a, buffer, Rights::READ).unwrap();
    let submit = table.derive(&mut objects, a, domain, Rights::SEND).unwrap();
    assert!(matches!(
        table.lookup(
            &mut objects,
            a,
            readonly,
            ObjectType::DmaBuffer,
            Rights::WRITE
        ),
        Err(Error::Rights)
    ));
    assert!(matches!(
        table.lookup(&mut objects, a, submit, ObjectType::DmaDomain, Rights::MAP),
        Err(Error::Rights)
    ));
    assert!(matches!(
        table.lookup(&mut objects, a, buffer, ObjectType::DmaDomain, Rights::SEND),
        Err(Error::Type)
    ));
    assert_eq!(table.retire_device_lease(&mut objects, 8), Ok(0));
    assert_eq!(table.retire_device_lease(&mut objects, 9), Ok(4));
    assert!(matches!(
        table.lookup(&mut objects, a, buffer, ObjectType::DmaBuffer, Rights::READ),
        Err(Error::Stale)
    ));
    assert_eq!(objects.stats(), Stats::default());
    assert_eq!(objects.live_grants(), 0);
}

#[test]
fn dma_allocation_capacity_and_invalid_inputs_leave_no_partial_grants() {
    let mut nodes = Nodes::new();
    let node = nodes.create().unwrap();
    let table = nodes.authority_mut(node).unwrap();
    let mut objects = Objects::with_devices();
    assert_eq!(
        objects.install_dma_capability(table, ObjectType::DmaDomain, 0),
        Err(Error::Object)
    );
    assert_eq!(
        objects.install_dma_capability(table, ObjectType::MmioRegion, 1),
        Err(Error::Type)
    );
    assert_eq!(objects.stats(), Stats::default());
    let resource = objects.device_resource().unwrap();
    for _ in 0..HANDLES {
        table
            .grant(&mut objects, &resource, Rights::INSPECT)
            .unwrap();
    }
    let before = objects.stats();
    for kind in [ObjectType::DmaDomain, ObjectType::DmaBuffer] {
        assert_eq!(
            objects.install_dma_capability(table, kind, 1),
            Err(Error::Capacity)
        );
        assert_eq!(objects.stats(), before);
        assert_eq!(objects.live_grants(), 0);
    }
    table.drain(&mut objects, node).unwrap();
    objects.release(resource).unwrap();
    assert_eq!(objects.stats(), Stats::default());
}

#[test]
fn strict_revoke_selects_dma_mapping_and_submission_descendants() {
    let mut nodes = Nodes::new();
    let node = nodes.create().unwrap();
    let table = nodes.authority_mut(node).unwrap();
    let mut objects = Objects::with_devices();
    let root = objects
        .install_dma_capability(table, ObjectType::DmaDomain, 1)
        .unwrap();
    let mapping = table.derive(&mut objects, node, root, Rights::MAP).unwrap();
    let submission = table
        .derive(&mut objects, node, root, Rights::SEND)
        .unwrap();
    let ids = [root, mapping, submission].map(|h| table.grant_id(&objects, node, h).unwrap());
    let ticket = table.begin_revoke(&mut objects, node, root).unwrap();
    assert_eq!(objects.revoke_selects(&ticket, ids[0]), Ok(false));
    for id in &ids[1..] {
        assert_eq!(objects.revoke_selects(&ticket, *id), Ok(true));
    }
    assert_eq!(table.purge_revoked(&mut objects), Ok(2));
    assert_eq!(objects.finish_revoke(&ticket), Ok(2));
    table.close(&mut objects, node, root).unwrap();
    assert_eq!(objects.live_grants(), 0);
    assert_eq!(objects.stats(), Stats::default());
}
