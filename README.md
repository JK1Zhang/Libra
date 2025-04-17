# Libra: Cooperative Scheduling Between CPU and Disk I/O for Load Balancing in Distributed Key-Value Stores

## 1. Introduction

Distributed key-value (KV) stores are fundamental components of modern computing infrastructure for efficiently storing and managing large-scale datasets.  Existing distributed KV stores often shard data by key ranges into multiple regions and distribute the regions across multiple nodes. However,  range-based sharding leads to load imbalance in two critical dimensions: CPU utilization and disk I/O. Also, the dynamic and often misaligned characteristics of the two dimensions make it challenging to simultaneously achieve balance in both.  We propose Libra, a cooperative scheduling framework that monitors the interactions of CPU and disk I/O loads and carefully migrates regions across nodes for load balancing. We implement Libra atop TiKV, a production distributed KV store, and show that Libra increases throughput by up to 72.1\% and reduces tail latency by up to 56.7\% compared to state-of-the-art approaches.




## 2. Overview
* The prototype is written in Golang based on [TiKV project]([TiKV Project](https://github.com/tikv))

* [The introduction on TiKV](./src/Libra_KV/README.md)

* [The introduction on PD](./src/Libra_PD/README.md)

  

## 3. Dependency 

See details in [Libra_PD ](./src/Libra_PD/README.md)and [Libra_KV](./src/Libra_KV/README.md).



## 3. Build and install Libra project

* Getting the source code of Libra  
`$ git clone git@github.com:JK1Zhang/Libra.git`

* Compile Libra 
  `$ cd src/Libra_KV`
  `$ make`
  `$ cd src/Libra_PD`
  `$ make`

  

## 4. Deploy the Libra Prototype

- Install tiup tools

  `$ curl --proto '=https' --tlsv1.2 -sSf https://tiup-mirrors.pingcap.com/install.sh | sh`

  `$ source .bash_profile`

  `$ tiup cluster`

- Topology setup and deploy

  Libra needs to setup clusters through a [topology file](https://tikv.org/docs/7.1/deploy/install/production/#step-2-initialize-cluster-topology-file)，there is an [example](./topology.yaml) in the repository as a reference.

  `$ tiup cluster deploy Libra v5.4.0 ./topology.yaml --user root [-p] [-i /home/root/.ssh/gcp_rsa]`

  `$ tiup cluster start Libra`

  `$./deploy.sh Libra`

  

## 5. Build and install benchmark

- Mixgraph benchmark

  `$ cd benchmark/Mixgraph`
  `$ make`

- YCSB benchmark

  `$ git clone https://github.com/pingcap/go-ycsb.git`

  `$ cd go-ycsb`
  `$ make`

  

## 6. Testing the Libra Prototype

Using YCSB on the client node to issue requests to the Libra cluster

**Load the database**

`$ ./go-ycsb load tikv -P workloads/workloada -p tikv.pd=$node IP: port$ -p threadcount=$N1$ -p operationcount=$N2$ ...`

**Run benchmarks based on the database**
``$ ./go-ycsb run tikv -P workloads/workloada -p tikv.pd=$node IP: port$ -p threadcount=$N1$ -p operationcount=$N2$ ...``

If you're planning to test the **Mixgraph** workload, go ahead and use the Mixgraph benchmark from this repository and add the following parameters to set it：

``$ -p mixgraph=true -p fieldlengthdistribution=pareto -p fieldlength=$N1$ -p fieldcount=1 -p keyrangenum=$N2$   -p insertorder=order -p zeropadding=20 -p valuesigma=226.409 -p valuek=0.923 -p keydista=0.002312 -p keydistb=0.3467 -p usedefaultrequest=false -p requestdistribution=zipfian -p keyrangedista=141.8  ...``

