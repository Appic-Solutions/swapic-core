#!/usr/bin/env bash
set -euo pipefail

show_help() {
  cat << EOF_HELP
The local ICP demo of the settlement canister's new role. A local dfx replica runs:
  - the settlement canister (this branch's production wasm);
  - the ICRC ledger wasm twice, as ckUSDC and as a second token (ckDEMO);
  - a MOCK ICPSwap pool (scripts/demo/mock_icpswap.mo), the calls the canister makes only;
  - a MOCK ck minter (scripts/demo/mock_minter.mo): it burns on withdraw_erc20 while this
    script releases the USDC on the Ethereum fork, and it mints when this script sees the
    ck helper's deposit on the fork.
Anvil forks of Ethereum, Base and Arbitrum carry the SwapicRouter and SwapicExecutor
deployed by the contracts branch's own script (DeploySwapic.s.sol), the canister set as the
Ethereum router's relayer. CCTP attestations come from the services branch's stand-in
attester (swapic-demo fake-iris), as in its fork demo.

Legs, each printed as a PASS line with its timings:
  1. ICP to EVM: ckUSDC on ICP to USDC on Arbitrum (pull, ck withdrawal, the canister's
     one Ethereum transaction through the router, CCTP to Arbitrum);
  2. EVM to ICP: USDC on Base to ckUSDC at the user's principal (router on Base, CCTP to
     the Ethereum executor, the ck helper deposit into the quote's subaccount, the
     canister forwards);
  3. EVM to ICP through the MOCK ICPSwap: USDC on Base to ckDEMO at the user's principal;
  4. the gasless relay: a user's signed swapFor on Base, relayed by the canister, CCTP to
     Arbitrum.

Run from the repository's root folder.

Usage:
  scripts/demo-icp.sh [options]

Options:
  -h, --help        Show this message and exit
  -n, --no-build    Use the settlement wasm already built

Environment:
  ALCHEMY_API_KEY   The forks' upstream. Read from RESEARCH_ENV when unset; never printed,
                    and handed to anvil through the environment, never its arguments.
  RESEARCH_ENV      Default: ../research/.env
  SWAPIC_CORE_P5    The contracts branch checkout. Default: ../swapic-core-p5
  SWAPIC_BACKEND_P6 The services branch checkout. Default: ../swapic-backend-p6
  LEDGER_WASM       The ICRC ledger wasm. Default: ../legacy/evm-minter/wasm/ledger_canister_u256.wasm.gz
  LEDGER_DID        Its interface. Default: ../legacy/evm-minter/did/ledger.did
  DEMO_LEGS         Which legs to run. Default: icp-evm,evm-icp,evm-icp-swap,relay
EOF_HELP
}

BUILD=1
while [[ $# -gt 0 && "$1" =~ ^- && "$1" != "--" ]]; do
  case $1 in
    -h | --help ) show_help; exit 0 ;;
    -n | --no-build ) BUILD=0 ;;
    * ) echo "Error: unknown option $1" >&2; show_help >&2; exit 1 ;;
  esac
  shift
done

REPO_ROOT=$(pwd)
[[ -f "$REPO_ROOT/scripts/demo-icp.sh" ]] || { echo "Error: run this from the repository's root folder" >&2; exit 1; }
RESEARCH_ENV=${RESEARCH_ENV:-../research/.env}
P5=$(cd "${SWAPIC_CORE_P5:-../swapic-core-p5}" && pwd)
P6=$(cd "${SWAPIC_BACKEND_P6:-../swapic-backend-p6}" && pwd)
LEDGER_WASM=${LEDGER_WASM:-../legacy/evm-minter/wasm/ledger_canister_u256.wasm.gz}
LEDGER_DID=${LEDGER_DID:-../legacy/evm-minter/did/ledger.did}
DEMO_LEGS=${DEMO_LEGS:-icp-evm,evm-icp,evm-icp-swap,relay}

ETH_PORT=28545; BASE_PORT=28546; ARB_PORT=28547; IRIS_PORT=28590; DFX_PORT=4950
ETH_RPC=http://127.0.0.1:$ETH_PORT
BASE_RPC=http://127.0.0.1:$BASE_PORT
ARB_RPC=http://127.0.0.1:$ARB_PORT
IRIS_URL=http://127.0.0.1:$IRIS_PORT

ETH_CHAIN=1; BASE_CHAIN=8453; ARB_CHAIN=42161
ETH_USDC=0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48
BASE_USDC=0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913
ARB_USDC=0xaf88d065e77c8cC2239327C5EDb3A432268e5831
TRANSMITTER=0x81D40F21F12A8F0E3252Bccb954D722d4c464B64
ZERO=0x0000000000000000000000000000000000000000
ZERO32=0x0000000000000000000000000000000000000000000000000000000000000000
PARAMS_T='(bytes32,address,address,uint256,uint256,(address,bytes),uint256,uint256,uint256,uint8,uint64,uint256,uint32,(bytes32,uint64,uint64,uint256,uint256),(uint8,bytes32,address,uint256,bytes32,bytes32,uint64,(address,bytes)))'
CK_DEPOSITED_TOPIC=0xc51acd1acc8ff382449f6368636049ecbab7dc1468db3233b9f1ca377d239ab8

# anvil's development accounts, by role. The keys are derived into files, never printed.
MNEMONIC="test test test test test test test test test test test junk"
DEPLOYER=$(cast wallet address --mnemonic "$MNEMONIC" --mnemonic-index 0)
USER_EVM=$(cast wallet address --mnemonic "$MNEMONIC" --mnemonic-index 1)
RECIPIENT=$(cast wallet address --mnemonic "$MNEMONIC" --mnemonic-index 2)
KEEPER=$(cast wallet address --mnemonic "$MNEMONIC" --mnemonic-index 3)
ATTESTER=$(cast wallet address --mnemonic "$MNEMONIC" --mnemonic-index 5)

for tool in anvil forge cast dfx cargo jq curl git tar perl python3; do
  command -v "$tool" > /dev/null || { echo "Error: $tool is not on the PATH" >&2; exit 1; }
done
[[ -f "$LEDGER_WASM" && -f "$LEDGER_DID" ]] || { echo "Error: no ledger wasm or did (LEDGER_WASM, LEDGER_DID)" >&2; exit 1; }
[[ -x "$P6/target/release/swapic-demo" ]] || { echo "Error: build the services branch's demo tool first (cargo build --release -p demo in $P6)" >&2; exit 1; }

WORK=$(mktemp -d "${TMPDIR:-/tmp}/swapic-demo-icp.XXXXXX")
LOGS=$WORK/logs; KEYS=$WORK/keys; DFXP=$WORK/dfx
mkdir -p "$LOGS" "$KEYS" "$DFXP"; chmod 700 "$KEYS"
# dfx's identities live in the run's own directory: the user's own identities are not touched
export DFX_CONFIG_ROOT=$WORK/dfxroot
export DFX_WARNING=-mainnet_plaintext_identity
mkdir -p "$DFX_CONFIG_ROOT"
ANVIL_TMP=$HOME/.foundry/anvil/tmp
mkdir -p "$ANVIL_TMP"
ANVIL_BEFORE=$(ls "$ANVIL_TMP")
PIDS=()
DFX_STARTED=0

cleanup() {
  status=$?
  set +e
  for pid in "${PIDS[@]:-}"; do
    if [[ -n "$pid" ]]; then kill "$pid" 2> /dev/null; fi
  done
  if [[ $DFX_STARTED == 1 ]]; then (cd "$DFXP" && dfx stop > /dev/null 2>&1); fi
  wait 2> /dev/null
  for dir in $(ls "$ANVIL_TMP" 2> /dev/null); do
    if ! grep -qx "$dir" <<< "$ANVIL_BEFORE"; then rm -rf "${ANVIL_TMP:?}/$dir"; fi
  done
  rm -rf "$KEYS" "$DFX_CONFIG_ROOT"
  echo "logs: $LOGS"
  exit $status
}
trap cleanup EXIT

step() { echo; echo "==> $*"; }
now() { perl -MTime::HiRes=time -e 'printf "%.1f\n", time'; }
since() { perl -e "printf '%.1f', $(now) - $1"; }
rpc() { curl -s -X POST -H 'content-type: application/json' --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":$3}" "$1"; }
wait_rpc() {
  for _ in $(seq 1 120); do
    rpc "$1" eth_blockNumber '[]' 2> /dev/null | grep -q result && return 0
    sleep 0.5
  done
  echo "Error: $1 did not answer" >&2; exit 1
}
on() { local url=$1; shift; ETH_RPC_URL=$url "$@"; }
send() { local url=$1; shift; on "$url" cast send --json "$@" | tee -a "$LOGS/cast-send.log" | jq -r .transactionHash; }
# dfx in the run's own project, as the admin identity unless told otherwise
dfxc() { (cd "$DFXP" && dfx "$@"); }
call() { dfxc canister call --identity admin "$@"; }
as_user() { dfxc canister call --identity user "$@"; }
args() { "$REPO_ROOT/target/debug/examples/demo_args" "$@"; }
blob() { local hex=${1#0x}; printf '%s' "$hex" | sed 's/../\\&/g'; }
rand32() { printf '0x%s' "$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')"; }
# the ck helper's principal encoding: a length byte, the bytes, zero padding to 32
principal32() {
  python3 -c "
import base64,sys
t=sys.argv[1].replace('-','').upper()
t+='='*((8-len(t)%8)%8)
b=base64.b32decode(t)[4:]
print('0x'+format(len(b),'02x')+b.hex()+'00'*(31-len(b)))
" "$1"
}
# the bytes of the first candid blob in a reply, as 0x hex
unblob() {
  local text
  text=$(grep -o "${2:-}blob \"\([^\"\\\\]\|\\\\.\)*\"" <<< "$1" | head -1 | sed 's/^.*blob "//; s/"$//')
  printf '0x%s' "$(perl -e '$s=shift; $o=""; while($s=~/\G(?:\\([0-9a-fA-F]{2})|\\(.)|(.))/gs){ $o .= defined $1 ? lc $1 : sprintf("%02x", ord(defined $2 ? $2 : $3)); } print $o' "$text")"
}
status_of() {
  call --query settlement get_flow "(blob \"$(blob "$1")\")" 2> /dev/null | grep -o 'status = "[A-Za-z]*"' | head -1 | cut -d'"' -f2
}
balance_of() {
  local ledger=$1 owner=$2
  dfxc canister call --identity admin --query "$ledger" icrc1_balance_of "(record { owner = principal \"$owner\"; subaccount = null })" \
    | tr -d '()_ \n' | sed 's/:nat$//'
}
wait_flow() {
  local hash=$1 want=$2 limit=$3 status=""
  for _ in $(seq 1 "$limit"); do
    status=$(status_of "$hash")
    [[ "$status" == "$want" ]] && return 0
    [[ "$status" == "Frozen" || "$status" == "Refunded" || "$status" == "Expired" ]] && break
    sleep 1
  done
  echo "    flow is $status, wanted $want" >&2
  call --query settlement get_flow "(blob \"$(blob "$hash")\")" >&2
  return 1
}

# ---------------------------------------------------------------------------------------
step "keys"
for i in 0 3 5; do
  cast wallet private-key "$MNEMONIC" "$i" > "$KEYS/k$i"
  chmod 600 "$KEYS/k$i"
done

step "forks of Ethereum, Base and Arbitrum"
if [[ -z "${ALCHEMY_API_KEY:-}" ]]; then
  ALCHEMY_API_KEY=$(grep '^ALCHEMY_API_KEY=' "$RESEARCH_ENV" | cut -d= -f2- | tr -d "\"'")
fi
[[ -n "$ALCHEMY_API_KEY" ]] || { echo "Error: no ALCHEMY_API_KEY" >&2; exit 1; }
# the services demo's bootstrap: each fork names an rpc alias whose foundry.toml entry is
# interpolated from the environment, so the url is in no argument
mkdir -p "$WORK/forks"
cat > "$WORK/forks/foundry.toml" << 'EOF_TOML'
[profile.default]
[rpc_endpoints]
upstream = "${ETH_RPC_URL}"
EOF_TOML
fork() {
  local port=$1 chain=$2 host=$3 extra=${4:-}
  # shellcheck disable=SC2086
  (cd "$WORK/forks" && ETH_RPC_URL="https://$host.g.alchemy.com/v2/$ALCHEMY_API_KEY" \
    exec anvil --quiet --port "$port" --chain-id "$chain" --fork-url upstream $extra > "$LOGS/anvil-$chain.log" 2>&1) &
  PIDS+=($!)
}
# Ethereum mines on a clock, so the 12 blocks a decision there waits for come by themselves
fork "$ETH_PORT" "$ETH_CHAIN" eth-mainnet "--block-time 1"
fork "$BASE_PORT" "$BASE_CHAIN" base-mainnet
fork "$ARB_PORT" "$ARB_CHAIN" arb-mainnet
unset ALCHEMY_API_KEY
wait_rpc "$ETH_RPC"; wait_rpc "$BASE_RPC"; wait_rpc "$ARB_RPC"
echo "    ethereum at $(on "$ETH_RPC" cast block-number), base at $(on "$BASE_RPC" cast block-number), arbitrum at $(on "$ARB_RPC" cast block-number)"

step "the settlement wasm and the demo tools"
if [[ $BUILD == 1 ]]; then
  scripts/build-canister.sh settlement > "$LOGS/build.log" 2>&1
fi
cargo build -q -p integration_tests --example demo_args
cp canister/target/wasm32-unknown-unknown/release/settlement.wasm "$DFXP/settlement.wasm"
cp canister/api/can.did "$DFXP/settlement.did"
cp "$LEDGER_WASM" "$DFXP/ledger.wasm.gz"
cp "$LEDGER_DID" "$DFXP/ledger.did"
cp scripts/demo/mock_minter.mo scripts/demo/mock_icpswap.mo "$DFXP/"
# the services branch's stand-in attester, copied so a rebuild there cannot move under us
cp "$P6/target/release/swapic-demo" "$WORK/swapic-demo"
cat > "$DFXP/dfx.json" << 'EOF_DFX'
{
  "canisters": {
    "settlement": { "type": "custom", "wasm": "settlement.wasm", "candid": "settlement.did",
      "metadata": [{ "name": "candid:service" }] },
    "ckusdc": { "type": "custom", "wasm": "ledger.wasm.gz", "candid": "ledger.did",
      "metadata": [{ "name": "candid:service" }] },
    "ckdemo": { "type": "custom", "wasm": "ledger.wasm.gz", "candid": "ledger.did",
      "metadata": [{ "name": "candid:service" }] },
    "mock_minter": { "type": "motoko", "main": "mock_minter.mo" },
    "mock_icpswap": { "type": "motoko", "main": "mock_icpswap.mo" }
  },
  "networks": { "local": { "bind": "127.0.0.1:4950", "type": "ephemeral" } }
}
EOF_DFX

step "a local replica"
dfxc identity new admin --storage-mode plaintext > /dev/null 2>&1
dfxc identity new user --storage-mode plaintext > /dev/null 2>&1
ADMIN=$(dfxc identity get-principal --identity admin)
USER_P=$(dfxc identity get-principal --identity user)
dfxc start --clean --background > "$LOGS/dfx.log" 2>&1
DFX_STARTED=1
for c in settlement ckusdc ckdemo mock_minter mock_icpswap; do
  dfxc canister create --identity admin "$c" > /dev/null 2>&1
  dfxc ledger fabricate-cycles --canister "$c" --t 500 > /dev/null 2>&1
done
SETTLEMENT=$(dfxc canister id settlement); CKUSDC=$(dfxc canister id ckusdc); CKDEMO=$(dfxc canister id ckdemo)
MINTER=$(dfxc canister id mock_minter); POOL=$(dfxc canister id mock_icpswap)
echo "    settlement $SETTLEMENT, ckUSDC $CKUSDC, ckDEMO $CKDEMO"
echo "    MOCK ck minter $MINTER, MOCK ICPSwap pool $POOL"
echo "    admin (controller, quoter, watcher) $ADMIN, user $USER_P"

ledger_arg() {
  cat << EOF_ARG
(variant { Init = record {
  minting_account = record { owner = principal "$MINTER"; subaccount = null };
  fee_collector_account = null;
  transfer_fee = 10_000 : nat;
  decimals = opt (6 : nat8);
  max_memo_length = opt (32 : nat16);
  token_symbol = "$1";
  token_name = "$1";
  metadata = vec {};
  initial_balances = vec {};
  feature_flags = opt record { icrc2 = true };
  maximum_number_of_accounts = null;
  accounts_overflow_trim_quantity = null;
  archive_options = record {
    num_blocks_to_archive = 1_000 : nat64; trigger_threshold = 2_000 : nat64;
    controller_id = principal "$ADMIN"; max_message_size_bytes = null;
    cycles_for_archive_creation = null; node_max_memory_size_bytes = null;
    max_transactions_per_response = null; more_controller_ids = null;
  };
} })
EOF_ARG
}
ledger_arg ckUSDC > "$WORK/ckusdc.arg"; ledger_arg ckDEMO > "$WORK/ckdemo.arg"
dfxc build --identity admin > "$LOGS/build-canisters.log" 2>&1
dfxc canister install --identity admin ckusdc --argument-file "$WORK/ckusdc.arg" >> "$LOGS/install.log" 2>&1
dfxc canister install --identity admin ckdemo --argument-file "$WORK/ckdemo.arg" >> "$LOGS/install.log" 2>&1
dfxc canister install --identity admin mock_minter >> "$LOGS/install.log" 2>&1
# ckUSDC is token0 and ckDEMO token1, two ckDEMO units to one ckUSDC unit
dfxc canister install --identity admin mock_icpswap --argument "(principal \"$CKUSDC\", principal \"$CKDEMO\", 2 : nat, 1 : nat)" >> "$LOGS/install.log" 2>&1
# the canister's chain urls go through files, never an argument
printf '%s\n' "$ETH_CHAIN=$ETH_RPC" "$BASE_CHAIN=$BASE_RPC" "$ARB_CHAIN=$ARB_RPC" > "$WORK/urls"
args init "$ADMIN" "$WORK/urls" "$WORK/init.hex"
rm -f "$WORK/urls"
dfxc canister install --identity admin settlement --argument-file "$WORK/init.hex" --argument-type raw >> "$LOGS/install.log" 2>&1
rm -f "$WORK/init.hex"
CANISTER_EVM=$(call settlement derive_evm_address | grep -o '0x[0-9a-fA-F]\{40\}')
echo "    the canister's Ethereum address $CANISTER_EVM"
# the allocator counts from zero and never reads a chain, so the address must be fresh there
for url in "$ETH_RPC" "$BASE_RPC" "$ARB_RPC"; do
  [[ "$(on "$url" cast nonce "$CANISTER_EVM")" == "0" ]] || { echo "Error: the canister's address has a nonce on $url" >&2; exit 1; }
  rpc "$url" anvil_setBalance "[\"$CANISTER_EVM\",\"0x8ac7230489e80000\"]" > /dev/null
done

step "contracts from $(git -C "$P5" rev-parse --short HEAD) of the contracts branch, by its deploy script"
git -C "$P5" archive HEAD contracts | tar -x -C "$WORK"
(cd "$WORK/contracts" && forge build -q > "$LOGS/forge-build.log" 2>&1)
deploy() {
  local url=$1 out
  out=$(cd "$WORK/contracts" && KEEPER="$KEEPER" RELAYER="$CANISTER_EVM" \
    forge script script/DeploySwapic.s.sol --rpc-url "$url" --broadcast --unlocked --sender "$DEPLOYER" 2>&1 | tee -a "$LOGS/deploy.log")
  echo "$(grep -o 'SWAPIC_ROUTER=0x[0-9a-fA-F]*' <<< "$out" | cut -d= -f2) $(grep -o 'SWAPIC_EXECUTOR=0x[0-9a-fA-F]*' <<< "$out" | cut -d= -f2)"
}
link() {
  (cd "$WORK/contracts" && forge script script/DeploySwapic.s.sol --sig "link(address,uint64,address)" "$2" "$3" "$4" \
    --rpc-url "$1" --broadcast --unlocked --sender "$DEPLOYER" >> "$LOGS/deploy.log" 2>&1)
}
read -r ETH_ROUTER ETH_EXECUTOR <<< "$(deploy "$ETH_RPC")"
read -r BASE_ROUTER BASE_EXECUTOR <<< "$(deploy "$BASE_RPC")"
read -r ARB_ROUTER ARB_EXECUTOR <<< "$(deploy "$ARB_RPC")"
[[ -n "$ETH_ROUTER" && -n "$BASE_ROUTER" && -n "$ARB_ROUTER" ]] || { echo "Error: a deploy failed, see $LOGS/deploy.log" >&2; exit 1; }
link "$ETH_RPC" "$ETH_ROUTER" "$ARB_CHAIN" "$ARB_EXECUTOR"
link "$BASE_RPC" "$BASE_ROUTER" "$ETH_CHAIN" "$ETH_EXECUTOR"
link "$BASE_RPC" "$BASE_ROUTER" "$ARB_CHAIN" "$ARB_EXECUTOR"
echo "    ethereum: router $ETH_ROUTER (relayer $(on "$ETH_RPC" cast call "$ETH_ROUTER" 'relayer()(address)')), executor $ETH_EXECUTOR"
echo "    base: router $BASE_ROUTER; arbitrum: router $ARB_ROUTER"

step "a stand-in CCTP attester on the Ethereum and Arbitrum forks"
for url in "$ETH_RPC" "$ARB_RPC"; do
  manager=$(on "$url" cast call "$TRANSMITTER" "attesterManager()(address)")
  rpc "$url" anvil_impersonateAccount "[\"$manager\"]" > /dev/null
  rpc "$url" anvil_setBalance "[\"$manager\",\"0x8ac7230489e80000\"]" > /dev/null
  send "$url" --unlocked --from "$manager" "$TRANSMITTER" "enableAttester(address)" "$ATTESTER" > /dev/null
  send "$url" --unlocked --from "$manager" "$TRANSMITTER" "setSignatureThreshold(uint256)" 1 > /dev/null
  rpc "$url" anvil_stopImpersonatingAccount "[\"$manager\"]" > /dev/null
done
"$WORK/swapic-demo" fake-iris --port "$IRIS_PORT" --node "0=$ETH_RPC" --node "6=$BASE_RPC" \
  --attester-key-file "$KEYS/k5" > "$LOGS/fake-iris.log" 2>&1 &
PIDS+=($!)
attested() {
  local domain=$1 tx=$2 answer
  for _ in $(seq 1 60); do
    answer=$(curl -s "$IRIS_URL/v2/messages/$domain?transactionHash=$tx")
    if jq -e '.messages[0].attestation' <<< "$answer" > /dev/null 2>&1; then echo "$answer"; return 0; fi
    sleep 1
  done
  echo "Error: no attestation for $tx" >&2; return 1
}

step "the canister's flow settings and chain data"
call settlement set_flow_settings "$(args flow-settings "$ETH_ROUTER" "$BASE_ROUTER" "$ARB_ROUTER" "$ETH_USDC" "$CKUSDC" "$MINTER" "$POOL")" --type raw > /dev/null
push_chain() {
  local chain=$1 url=$2 block base
  block=$(on "$url" cast block-number)
  base=$(on "$url" cast base-fee)
  call settlement push_chain_data "($chain : nat64, record { block = $block : nat64; base_fee_wei_per_gas = $base : nat; priority_fee_wei_per_gas = 1_000_000_000 : nat })" > /dev/null
}
push_chain "$ETH_CHAIN" "$ETH_RPC"; push_chain "$BASE_CHAIN" "$BASE_RPC"; push_chain "$ARB_CHAIN" "$ARB_RPC"
echo "    ok"

step "balances: the user's ckUSDC, the pool's liquidity, the EVM accounts' USDC"
call mock_minter mock_mint "(principal \"$CKUSDC\", record { owner = principal \"$USER_P\"; subaccount = null }, 50_000_000 : nat)" > /dev/null
call mock_minter mock_mint "(principal \"$CKUSDC\", record { owner = principal \"$POOL\"; subaccount = null }, 50_000_000 : nat)" > /dev/null
call mock_minter mock_mint "(principal \"$CKDEMO\", record { owner = principal \"$POOL\"; subaccount = null }, 100_000_000 : nat)" > /dev/null
"$WORK/swapic-demo" fund --rpc "$BASE_RPC" --token "$BASE_USDC" --to "$USER_EVM" --amount 1000000000 >> "$LOGS/fund.log" 2>&1
"$WORK/swapic-demo" fund --rpc "$ETH_RPC" --token "$ETH_USDC" --to "$DEPLOYER" --amount 1000000000 >> "$LOGS/fund.log" 2>&1
echo "    user ckUSDC $(balance_of ckusdc "$USER_P")"

PASSED=0; FAILED=0
pass() { echo "PASS $*"; PASSED=$((PASSED + 1)); }
fail() { echo "FAIL $*"; FAILED=$((FAILED + 1)); }

# ---------------------------------------------------------------------------------------
leg_icp_to_evm() {
  step "leg: ICP to EVM, ckUSDC on ICP to USDC on Arbitrum"
  local t0 amount=5000000 max_fee=5000 min_out=4900000 deadline expires route qh fee arb_before arb_after
  local t_pull t_land t_cont tx answer message attestation
  t0=$(now)
  deadline=$(( $(date +%s) + 3600 ))
  expires=$(( $(date +%s) + 1800 ))
  # the route the user signed for: CCTP fast to Arbitrum, Deliver to the recipient. Its payer,
  # input and deadline are the canister's to fill in at landing.
  route=$(cast abi-encode "f($PARAMS_T)" \
    "($(rand32),$ZERO,$ETH_USDC,$amount,$deadline,($ZERO,0x),$amount,0,0,0,$ARB_CHAIN,$max_fee,1000,($ZERO32,0,0,0,0),(0,0x000000000000000000000000${RECIPIENT#0x},$ZERO,$min_out,$ZERO32,$ZERO32,$deadline,($ZERO,0x)))")
  answer=$(call settlement register_icp_quote "$(args icp-quote "$USER_P" "$CKUSDC" "$amount" 4900000 "$route" "$expires" 1)" --type raw)
  qh=$(unblob "$answer")
  echo "    quote $qh registered"
  fee=10000
  # the user approves the canister with the quote hash as the spender subaccount
  as_user ckusdc icrc2_approve "(record { spender = record { owner = principal \"$SETTLEMENT\"; subaccount = opt blob \"$(blob "$qh")\" }; amount = $((amount + fee)) : nat })" > /dev/null
  arb_before=$(on "$ARB_RPC" cast call "$ARB_USDC" "balanceOf(address)(uint256)" "$RECIPIENT" | cut -d' ' -f1)
  call settlement start_icp_swap "(blob \"$(blob "$qh")\")" > "$LOGS/start.log"
  t_pull=$(since "$t0")
  echo "    pulled after ${t_pull}s"
  # the MOCK minter burned; release the USDC on the Ethereum fork as the real minter would
  local pending id recipient value
  for _ in $(seq 1 60); do
    pending=$(call --query mock_minter mock_pending)
    grep -q withdrawal_id <<< "$pending" && break
    sleep 1
  done
  id=$(grep -o 'withdrawal_id = [0-9_]*' <<< "$pending" | head -1 | grep -o '[0-9_]*$' | tr -d _)
  recipient=$(grep -o 'recipient_address = "[^"]*"' <<< "$pending" | head -1 | cut -d'"' -f2)
  value=$(grep -o 'withdrawal_amount = [0-9_]*' <<< "$pending" | head -1 | grep -o '[0-9_]*$' | tr -d _)
  [[ -n "$id" ]] || { fail "icp-to-evm: no withdrawal reached the MOCK minter"; return; }
  tx=$(send "$ETH_RPC" --unlocked --from "$DEPLOYER" "$ETH_USDC" "transfer(address,uint256)" "$recipient" "$value")
  call mock_minter mock_finalize "($id : nat64, \"$tx\")" > /dev/null
  echo "    withdrawal $id of $value released to $recipient in $tx"
  wait_flow "$qh" Continuing 120 || wait_flow "$qh" Done 5 || { fail "icp-to-evm: the continuation never went out"; return; }
  t_land=$(since "$t0")
  echo "    landed and continued after ${t_land}s"
  wait_flow "$qh" Done 120 || { fail "icp-to-evm: the continuation never confirmed"; return; }
  t_cont=$(since "$t0")
  tx=$(unblob "$(call --query settlement get_flow "(blob \"$(blob "$qh")\")")" "tx_hash = opt ")
  echo "    the canister's Ethereum transaction $tx confirmed after ${t_cont}s"
  answer=$(attested 0 "$tx") || { fail "icp-to-evm: no attestation"; return; }
  message=$(jq -r '.messages[0].message' <<< "$answer")
  attestation=$(jq -r '.messages[0].attestation' <<< "$answer")
  send "$ARB_RPC" --unlocked --from "$DEPLOYER" "$TRANSMITTER" "receiveMessage(bytes,bytes)" "$message" "$attestation" > /dev/null
  arb_after=$(on "$ARB_RPC" cast call "$ARB_USDC" "balanceOf(address)(uint256)" "$RECIPIENT" | cut -d' ' -f1)
  if (( arb_after > arb_before )); then
    pass "icp-to-evm: $amount ckUSDC on ICP -> $((arb_after - arb_before)) USDC on Arbitrum; pull ${t_pull}s, landing+continuation ${t_land}s, confirmed ${t_cont}s, delivered $(since "$t0")s"
  else
    fail "icp-to-evm: nothing arrived on Arbitrum"
  fi
}

# Base router -> CCTP -> Ethereum executor -> ck helper -> MOCK minter -> canister -> user
evm_to_icp() {
  local name=$1 target=$2 pool=$3 min_out=$4
  step "leg: EVM to ICP, USDC on Base to $name at the user's principal"
  local t0 amount=5000000 max_fee=50000 ref answer subaccount principal deadline params tx message attestation receipt
  local before after t_burn t_mint target_ledger
  t0=$(now)
  ref=$(rand32)
  answer=$(call settlement register_evm_quote "$(args evm-quote "$USER_P" "$target" "$pool" "$min_out" "$BASE_CHAIN" $(( $(date +%s) + 3600 )) 2 "$ref")" --type raw)
  grep -q Ok <<< "$answer" || { fail "$name: registration refused: $answer"; return; }
  principal=$(principal32 "$SETTLEMENT")
  deadline=$(( $(date +%s) + 3600 ))
  # CkDeposit on Ethereum into the canister's subaccount named by the ref; the recipient is
  # the user's Ethereum fallback address
  params="($ref,$USER_EVM,$BASE_USDC,$amount,$(( $(date +%s) + 600 )),($ZERO,0x),$amount,0,0,0,$ETH_CHAIN,$max_fee,1000,($ZERO32,0,0,0,0),(2,0x000000000000000000000000${USER_EVM#0x},$ZERO,$((amount - max_fee)),$principal,$ref,$deadline,($ZERO,0x)))"
  target_ledger=ckusdc; [[ "$target" == "$CKDEMO" ]] && target_ledger=ckdemo
  before=$(balance_of "$target_ledger" "$USER_P")
  rpc "$BASE_RPC" anvil_impersonateAccount "[\"$USER_EVM\"]" > /dev/null
  send "$BASE_RPC" --unlocked --from "$USER_EVM" "$BASE_USDC" "approve(address,uint256)" "$BASE_ROUTER" "$amount" > /dev/null
  tx=$(send "$BASE_RPC" --unlocked --from "$USER_EVM" "$BASE_ROUTER" "swap($PARAMS_T)" "$params")
  t_burn=$(since "$t0")
  echo "    the user's swap on Base $tx after ${t_burn}s"
  answer=$(attested 6 "$tx") || { fail "$name: no attestation"; return; }
  message=$(jq -r '.messages[0].message' <<< "$answer")
  attestation=$(jq -r '.messages[0].attestation' <<< "$answer")
  # the keeper completes on the Ethereum executor, which makes the ck helper deposit
  tx=$(send "$ETH_RPC" --unlocked --from "$KEEPER" "$ETH_EXECUTOR" "completeCctp(bytes,bytes,(address,bytes))" "$message" "$attestation" "($ZERO,0x)")
  receipt=$(on "$ETH_RPC" cast receipt --json "$tx")
  local log principal_t subaccount_t value
  log=$(jq -c --arg t "$CK_DEPOSITED_TOPIC" '[.logs[] | select(.topics[0] == $t)][0]' <<< "$receipt")
  [[ "$log" != "null" ]] || { fail "$name: the executor made no ck deposit"; return; }
  principal_t=$(jq -r '.topics[2]' <<< "$log")
  subaccount_t=0x$(jq -r '.data' <<< "$log" | cut -c3-66)
  value=$(cast to-dec 0x$(jq -r '.data' <<< "$log" | cut -c67-130))
  echo "    ck helper deposit of $value into subaccount $subaccount_t"
  # the MOCK minter mints, as the real one does on the helper's event
  call mock_minter mock_mint_deposit "(principal \"$CKUSDC\", blob \"$(blob "$principal_t")\", blob \"$(blob "$subaccount_t")\", $value : nat)" > /dev/null
  t_mint=$(since "$t0")
  wait_flow "$ref" Done 90 || { fail "$name: the canister did not pay the user"; return; }
  after=$(balance_of "$target_ledger" "$USER_P")
  if (( after > before )); then
    pass "$name: $amount USDC on Base -> $((after - before)) $name at the user's principal; burn ${t_burn}s, minted ${t_mint}s, paid $(since "$t0")s"
  else
    fail "$name: the user's balance did not move"
  fi
}

# a user on Base signs, the canister relays and pays the gas, the router reimburses it in USDC
leg_relay() {
  step "leg: gasless relay, USDC on Base to USDC on Arbitrum, the canister relaying"
  local t0 amount=3000000 gas_fee=50000 max_fee=5000 deadline params params_hex qh separator struct digest sig auth answer
  local arb_before arb_after status tx message attestation wallet payer keystore
  t0=$(now)
  # a fresh payer in a keystore: anvil's well-known accounts carry EIP-7702 code on the real
  # chains, and USDC then checks their signatures through ERC-1271 instead of ECDSA
  wallet=$(cast wallet new "$KEYS" --unsafe-password demo --json)
  payer=$(jq -r '.[0].address' <<< "$wallet")
  keystore=$(jq -r '.[0].path' <<< "$wallet")
  "$WORK/swapic-demo" fund --rpc "$BASE_RPC" --token "$BASE_USDC" --to "$payer" --amount "$amount" >> "$LOGS/fund.log" 2>&1
  deadline=$(( $(date +%s) + 600 ))
  params="($(rand32),$payer,$BASE_USDC,$amount,$deadline,($ZERO,0x),$amount,0,100000,0,$ARB_CHAIN,$max_fee,1000,($ZERO32,0,0,0,0),(0,0x000000000000000000000000${RECIPIENT#0x},$ZERO,$((amount - gas_fee - max_fee)),$ZERO32,$ZERO32,$(( deadline + 3600 )),($ZERO,0x)))"
  params_hex=$(cast abi-encode "f($PARAMS_T)" "$params")
  qh=$(on "$BASE_RPC" cast call "$BASE_ROUTER" "quoteHash($PARAMS_T)(bytes32)" "$params")
  # EIP-3009 receiveWithAuthorization, its nonce the quote hash, under USDC's own domain
  separator=$(on "$BASE_RPC" cast call "$BASE_USDC" "DOMAIN_SEPARATOR()(bytes32)")
  struct=$(cast keccak "$(cast abi-encode "f(bytes32,address,address,uint256,uint256,uint256,bytes32)" \
    0xd099cc98ef71107a616c4f0f941f04c322d8e254fe26b3c6668db87aae413de8 "$payer" "$BASE_ROUTER" "$amount" 0 "$deadline" "$qh")")
  digest=$(cast keccak "0x1901${separator#0x}${struct#0x}")
  sig=$(cast wallet sign --no-hash --keystore "$keystore" --password demo "$digest")
  auth=$(cast abi-encode "f((uint8,uint256,uint256,uint256,uint256,uint256,bytes,bytes))" "(0,$gas_fee,0,$deadline,0,0,$sig,0x)")
  arb_before=$(on "$ARB_RPC" cast call "$ARB_USDC" "balanceOf(address)(uint256)" "$RECIPIENT" | cut -d' ' -f1)
  answer=$(call settlement relay_gasless "$(args relay "$BASE_CHAIN" "$params_hex" "$auth")" --type raw)
  grep -q Ok <<< "$answer" || { fail "gasless-relay: refused: $answer"; return; }
  echo "    relayed quote $qh after $(since "$t0")s"
  for _ in $(seq 1 60); do
    status=$(call --query settlement relay_status "(blob \"$(blob "$qh")\")")
    grep -q 'Confirmed\|Failed' <<< "$status" && break
    sleep 1
  done
  grep -q Confirmed <<< "$status" || { fail "gasless-relay: the relay did not confirm: $status"; return; }
  tx=$(unblob "$status")
  echo "    the canister's swapFor $tx confirmed after $(since "$t0")s; its USDC on Base: $(on "$BASE_RPC" cast call "$BASE_USDC" "balanceOf(address)(uint256)" "$CANISTER_EVM" | cut -d' ' -f1)"
  answer=$(attested 6 "$tx") || { fail "gasless-relay: no attestation"; return; }
  message=$(jq -r '.messages[0].message' <<< "$answer")
  attestation=$(jq -r '.messages[0].attestation' <<< "$answer")
  send "$ARB_RPC" --unlocked --from "$DEPLOYER" "$TRANSMITTER" "receiveMessage(bytes,bytes)" "$message" "$attestation" > /dev/null
  arb_after=$(on "$ARB_RPC" cast call "$ARB_USDC" "balanceOf(address)(uint256)" "$RECIPIENT" | cut -d' ' -f1)
  if (( arb_after > arb_before )); then
    pass "gasless-relay: $amount USDC on Base, signed by a payer with no ETH -> $((arb_after - arb_before)) USDC on Arbitrum, the canister relaying; delivered $(since "$t0")s"
  else
    fail "gasless-relay: nothing arrived on Arbitrum"
  fi
}

[[ ",$DEMO_LEGS," == *",icp-evm,"* ]] && leg_icp_to_evm
[[ ",$DEMO_LEGS," == *",evm-icp,"* ]] && evm_to_icp "ckUSDC" "$CKUSDC" - 1
[[ ",$DEMO_LEGS," == *",evm-icp-swap,"* ]] && evm_to_icp "ckDEMO" "$CKDEMO" "$POOL" 1000000
[[ ",$DEMO_LEGS," == *",relay,"* ]] && leg_relay

echo
echo "demo: $PASSED passed, $FAILED failed"
[[ $FAILED -eq 0 ]]
