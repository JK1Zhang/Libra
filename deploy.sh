cluster=$1
if [ "$1" = "" ]; then
    echo cluster name should not be empty!
    echo "help: $0 [Libra]"
    exit
fi

set -x

echo update tikv binary of cluster: $cluster

mkdir -p tmp
cp ./Libra_KV/target/release/tikv-server ./tmp
cd tmp
tar zcf tikv-server.tar.gz tikv-server
tiup cluster patch $cluster ./tikv-server.tar.gz -R tikv
rm tikv-server tikv-server.tar.gz


set -x

echo update pd binary of cluster: $cluster

cd ./Libra_pd/bin
tar zcf pd-server.tar.gz pd-server
tiup cluster patch $cluster ./pd-server.tar.gz -R pd
rm pd-server.tar.gz