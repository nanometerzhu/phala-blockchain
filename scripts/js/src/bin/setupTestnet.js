const { program } = require('commander');
const {ApiPromise, WsProvider, Keyring} = require('@polkadot/api');
const Phala = require('@phala/sdk');

const { TxQueue, checkUntil, hex } = require('../utils/tx');

program
    .option('--ws-endpoint <endpoint>', 'substrate ws rpc endpoint', 'ws://localhost:19944')
    .option('--root-key <key>', 'root key SURI', '//Alice')
    .option('--root-type <key-type>', 'root key type', 'sr25519')
    .option('--pruntime <pr-endpoint>', 'pruntime rpc endpoint', 'http://localhost:18000')
    .action(() =>
        main()
            .then(process.exit)
            .catch(console.error)
            .finally(() => process.exit(-1))
    )
    .parse(process.argv);

async function getWorkerPubkey(api) {
    const workers = await api.query.phalaRegistry.workers.entries();
    const worker = workers[0][0].args[0].toString();
    return worker;
}

async function setupGatekeeper(api, txpool, pair, worker) {
    if ((await api.query.phalaRegistry.gatekeeper()).length > 0) {
        return;
    }
    console.log('Gatekeeper: registering');
    await txpool.submit(
        api.tx.sudo.sudo(
            api.tx.phalaRegistry.registerGatekeeper(worker)
        ),
        pair,
    );
    await checkUntil(
        async () => (await api.query.phalaRegistry.gatekeeper()).length == 1,
        4 * 6000
    );
    console.log('Gatekeeper: added');
    await checkUntil(
        async () => (await api.query.phalaRegistry.gatekeeperMasterPubkey()).isSome,
        4 * 6000
    );
    console.log('Gatekeeper: master key ready');
}

async function deployCluster(api, txqueue, pair, worker, defaultCluster = '0x0000000000000000000000000000000000000000000000000000000000000000') {
    if ((await api.query.phalaRegistry.clusterKeys(defaultCluster)).isSome) {
        return defaultCluster;
    }
    console.log('Cluster: creating');
    // crete contract cluster and wait for the setup
    const { events } = await txqueue.submit(
        api.tx.phalaFatContracts.addCluster(
            'Public', // can be {'OnlyOwner': accountId}
            [worker]
        ),
        pair
    );
    const ev = events[1].event;
    console.assert(ev.section == 'phalaFatContracts' && ev.method == 'ClusterCreated');
    const clusterId = ev.data[0].toString();
    console.log('Cluster: created', clusterId)
    await checkUntil(
        async () => (await api.query.phalaRegistry.clusterKeys(clusterId)).isSome,
        4 * 6000
    );
    return clusterId;
}

async function main() {
    const { endpoint, key, keyType, pruntime: pruntimUrl } = program.opts();
    // Connect to the chain
    const wsProvider = new WsProvider(endpoint);
    const api = await ApiPromise.create({provider: wsProvider});
    const txqueue = new TxQueue(api);

    // Prepare accounts
    const keyring = new Keyring({type: keyType});
    const alice = keyring.addFromUri(key);

    // Connect to pruntime
    const prpc = Phala.createPruntimeApi(pruntimUrl);
    const worker = await getWorkerPubkey(api);
    const connectedWorker = hex((await prpc.getInfo({})).publicKey);
    console.log('Worker:', worker);
    console.log('Connected worker:', connectedWorker);

    // basic phala network setup
    await setupGatekeeper(api, txqueue, alice, worker);
    await deployCluster(api, txqueue, alice, worker);
}

