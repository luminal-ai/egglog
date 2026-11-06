use crate::{
    action::mask::{IterResult, ValueSource},
    pool::{PoolSet, with_pool_set},
};

use super::mask::{Mask, MaskIter};

#[test]
fn sparse_mutation_buffers_keep_notifications_and_flush_order() {
    use super::{MutationBuffers, TableId, Value};
    use crate::numeric_id::NumericId;
    use crate::table_spec::MutationBuffer;
    use std::sync::{Arc, Mutex};

    type Flushed = Arc<Mutex<Vec<(u32, Vec<(&'static str, Vec<Value>)>)>>>;
    struct Recording {
        id: u32,
        pending: Vec<(&'static str, Vec<Value>)>,
        flushed: Flushed,
    }
    impl MutationBuffer for Recording {
        fn stage_insert(&mut self, row: &[Value]) {
            self.pending.push(("insert", row.to_vec()));
        }
        fn stage_remove(&mut self, row: &[Value]) {
            self.pending.push(("remove", row.to_vec()));
        }
        fn fresh_handle(&self) -> Box<dyn MutationBuffer> {
            Box::new(Self {
                id: self.id,
                pending: Vec::new(),
                flushed: self.flushed.clone(),
            })
        }
    }
    impl Drop for Recording {
        fn drop(&mut self) {
            self.flushed
                .lock()
                .unwrap()
                .push((self.id, std::mem::take(&mut self.pending)));
        }
    }
    let notifications = Default::default();
    let flushed = Flushed::default();
    let mut buffers = MutationBuffers::new(&notifications, Default::default());
    let low = TableId::new(3);
    let high = TableId::new(65_535);
    for id in [high, low] {
        buffers.lazy_init(id, || {
            Box::new(Recording {
                id: if id == high { 65_535 } else { 3 },
                pending: Vec::new(),
                flushed: flushed.clone(),
            })
        });
    }
    buffers.lazy_init(high, || panic!("existing buffer was replaced"));
    buffers.stage_insert(high, &[Value::new(11)]);
    buffers.stage_remove(low, &[Value::new(7)]);
    assert_eq!(notifications.reset().as_slice(), &[high, low]);
    let mut cloned = buffers.clone();
    cloned.stage_insert(high, &[Value::new(13)]);
    assert_eq!(notifications.reset().as_slice(), &[high]);
    drop(cloned);
    drop(buffers);
    assert_eq!(
        *flushed.lock().unwrap(),
        vec![
            (3, vec![]),
            (65_535, vec![("insert", vec![Value::new(13)])]),
            (3, vec![("remove", vec![Value::new(7)])]),
            (65_535, vec![("insert", vec![Value::new(11)])]),
        ]
    );
}

#[test]
fn mask_iter() {
    let ps = PoolSet::default();
    let offs = Vec::from_iter(0..100);
    let mut mask = Mask::new(0..100, &ps);
    let mut res = Vec::new();
    mask.iter(&offs).for_each(|x| res.push(*x));
    assert_eq!(offs, res);
}

#[test]
fn mask_iter_zip() {
    let ps = PoolSet::default();
    let offs1 = Vec::from_iter(0..100);
    let offs2 = Vec::from_iter(100..200);
    let mut mask = Mask::new(0..100, &ps);
    let mut res = Vec::new();
    mask.iter(&offs1)
        .zip(&offs2)
        .for_each(|(x, y)| res.push((*x, *y)));
    assert_eq!(
        Vec::from_iter(offs1.iter().copied().zip(offs2.iter().copied())),
        res
    );
}

#[test]
fn mask_iter_dyn() {
    let ps = PoolSet::default();
    let mut mask = Mask::new(0..3, &ps);
    let mut iter_dyn = mask.iter_dynamic(
        with_pool_set(|x| x.get_pool()),
        vec![
            ValueSource::Const(1),
            ValueSource::Slice(&[1, 3, 5]),
            ValueSource::Const(1),
            ValueSource::Slice(&[2, 4, 6]),
        ]
        .into_iter(),
    );
    match iter_dyn.get_at(2) {
        IterResult::Item(item) => {
            let v = item.as_slice();
            assert_eq!(v, [1, 5, 1, 6])
        }
        _ => unreachable!(),
    }
}

#[test]
fn retain() {
    let ps = PoolSet::default();
    let offs = Vec::from_iter(0..100);
    let mut mask = Mask::new(0..100, &ps);
    mask.iter(&offs).retain(|x| *x % 2 == 0);
    let mut got = Vec::new();
    mask.iter(&offs).for_each(|x| got.push(*x));
    assert_eq!(
        Vec::from_iter(offs.iter().copied().filter(|x| *x % 2 == 0)),
        got
    );
}

#[test]
fn fill_vec() {
    let ps = PoolSet::default();
    let offs = Vec::from_iter(0..100);
    let mut mask = Mask::new(0..100, &ps);
    let mut out = Vec::new();
    mask.iter(&offs).fill_vec(
        &mut out,
        || i32::MAX,
        |row, x| {
            assert_eq!(row, *x as usize);
            if *x % 2 == 0 { Some(*x) } else { None }
        },
    );
    // We should filter the mas for the entries for which we returned 'None'
    let mut got = Vec::new();
    mask.iter(&offs).for_each(|x| got.push(*x));
    assert_eq!(
        Vec::from_iter(offs.iter().copied().filter(|x| *x % 2 == 0)),
        got
    );

    assert_eq!(out.len(), 100);

    // The vector itself should have i32::MAX in for the odd indexes.
    for (i, x) in out.iter().copied().enumerate() {
        if i.is_multiple_of(2) {
            assert_eq!(x, i as i32);
        } else {
            assert_eq!(x, i32::MAX);
        }
    }
}

#[test]
fn test_early_stop_initial_state() {
    empty_execution_state!(state);
    assert!(!state.should_stop());
}

#[test]
fn test_early_stop_trigger() {
    empty_execution_state!(state);
    assert!(!state.should_stop());

    state.trigger_early_stop();

    assert!(state.should_stop());
}

#[test]
fn test_early_stop_shared_across_clones() {
    empty_execution_state!(state1);
    let state2 = state1.clone();

    assert!(!state1.should_stop());
    assert!(!state2.should_stop());

    state1.trigger_early_stop();

    assert!(state1.should_stop());
    assert!(state2.should_stop());
}

#[test]
fn test_early_stop_multiple_clones() {
    empty_execution_state!(state1);
    let state2 = state1.clone();
    let state3 = state2.clone();

    assert!(!state1.should_stop());
    assert!(!state2.should_stop());
    assert!(!state3.should_stop());

    state2.trigger_early_stop();

    assert!(state1.should_stop());
    assert!(state2.should_stop());
    assert!(state3.should_stop());
}
