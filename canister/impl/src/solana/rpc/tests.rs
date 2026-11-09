//! The SOL RPC canister's real interface (`sol_rpc.did`, fetched from `tghme-zyaaa-aaaar-qarca-cai`) against
//! the types here: every answer it can give must decode into ours, every request of ours must be one
//! it takes. So a change of its interface fails here, not on a swap.

use super::*;
use candid::types::subtype::{subtype, Gamma};
use candid::types::{internal::TypeContainer, Type, TypeEnv, TypeInner};
use candid_parser::IDLProg;

fn interface() -> (
    TypeEnv,
    candid::types::Function,
    Vec<(String, candid::types::Function)>,
) {
    let prog: IDLProg = include_str!("sol_rpc.did")
        .parse()
        .expect("the interface parses");
    let mut env = TypeEnv::new();
    let actor = candid_parser::check_prog(&mut env, &prog)
        .expect("the interface checks")
        .expect("a service");
    let service = env.as_service(&actor).expect("a service type");
    let methods: Vec<(String, candid::types::Function)> = service
        .iter()
        .map(|(name, ty)| (name.clone(), env.as_func(ty).expect("a method").clone()))
        .collect();
    let first = methods[0].1.clone();
    (env, first, methods)
}

fn method(methods: &[(String, candid::types::Function)], name: &str) -> candid::types::Function {
    methods
        .iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("no method {name}"))
        .1
        .clone()
}

/// `T` as a type in its own environment, merged into `env`.
fn ours<T: CandidType>(env: &mut TypeEnv) -> Type {
    let mut container = TypeContainer::new();
    let ty = container.add::<T>();
    for (name, inner) in container.env.0.iter() {
        // our names may shadow the interface's: rename by prefix
        env.0.insert(name.clone(), inner.clone());
    }
    ty
}

fn answer_decodes<T: CandidType>(method_name: &str) {
    let (mut env, _, methods) = interface();
    let real = method(&methods, method_name).rets[0].clone();
    let mine = ours::<T>(&mut env);
    let mut gamma = Gamma::new();
    subtype(&mut gamma, &env, &real, &mine)
        .unwrap_or_else(|e| panic!("{method_name}'s answer does not decode into ours: {e}"));
}

fn request_accepted<T: CandidType>(method_name: &str, position: usize) {
    let (mut env, _, methods) = interface();
    let real = method(&methods, method_name).args[position].clone();
    let mine = ours::<T>(&mut env);
    let mut gamma = Gamma::new();
    subtype(&mut gamma, &env, &mine, &real)
        .unwrap_or_else(|e| panic!("{method_name} does not take our argument {position}: {e}"));
}

#[test]
fn the_answers_we_read_decode_from_the_real_interface() {
    answer_decodes::<Multi<Option<AccountInfo>>>("getAccountInfo");
    answer_decodes::<Multi<u64>>("getBalance");
    answer_decodes::<Multi<Option<Confirmed>>>("getTransaction");
    answer_decodes::<Answer<Nat>>("getAccountInfoCyclesCost");
    answer_decodes::<Answer<Nat>>("getBalanceCyclesCost");
    answer_decodes::<Answer<Nat>>("getTransactionCyclesCost");
}

#[test]
fn the_requests_we_send_are_ones_it_takes() {
    for name in ["getAccountInfo", "getBalance", "getTransaction"] {
        request_accepted::<RpcSources>(name, 0);
        request_accepted::<Option<RpcConfig>>(name, 1);
    }
    request_accepted::<AccountParams>("getAccountInfo", 2);
    request_accepted::<BalanceParams>("getBalance", 2);
    request_accepted::<TransactionParams>("getTransaction", 2);
}
