use super::*;
use crate::storage::on_fresh_memory;
use types::{ChainId, UnixSeconds};

fn qh(byte: u8) -> QuoteHash {
    QuoteHash::new([byte; 32])
}

fn intent(destination: ChainId) -> EcoIntent {
    EcoIntent::new(
        destination,
        vec![0xde, 0xad],
        UnixSeconds::new(1_788_357_691),
        "0xeC00008537c1F26E739486BCFCC818d81234d5aD"
            .parse()
            .unwrap(),
    )
    .unwrap()
}

/// One intent per swap: a push lands, the same push again changes nothing, a corrected one
/// replaces it, and the engine takes it out when the swap closes.
#[test]
fn a_push_lands_replaces_and_is_taken_out() {
    on_fresh_memory(|| {
        init();
        assert_eq!(get(qh(1)), None);
        put(qh(1), intent(ChainId::BASE));
        assert_eq!(get(qh(1)), Some(intent(ChainId::BASE)));
        put(qh(1), intent(ChainId::BASE));
        assert_eq!(get(qh(1)), Some(intent(ChainId::BASE)), "idempotent");
        put(qh(1), intent(ChainId::ARBITRUM));
        assert_eq!(
            get(qh(1)),
            Some(intent(ChainId::ARBITRUM)),
            "a corrected push replaces"
        );
        assert_eq!(get(qh(2)), None);
        remove(qh(1));
        assert_eq!(get(qh(1)), None);
    });
}
