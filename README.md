# Libra



## Dependence

golang?

cargo?

make?





## Compile



```shell
cd src/Libra_PD；
make

cd src/Libra_KV
make
```



## Deploy





## Run



### 1.软硬件配置需求

[TiDB 软件和硬件环境建议配置 | PingCAP Docs](https://docs.pingcap.com/zh/tidb/stable/hardware-and-software-requirements)

[TiDB 环境与系统配置检查 | PingCAP Docs](https://docs.pingcap.com/zh/tidb/stable/check-before-deployment)



### 2.在中控机上部署 TiUP 组件

>  TiUP 组件是集群部署与管理工具，tiup只需要安装一次。

#### 2.1 执行如下命令安装 TiUP 工具：

```sh
curl --proto '=https' --tlsv1.2 -sSf https://tiup-mirrors.pingcap.com/install.sh | sh
```

#### 2.2 声明全局环境变量

```shell
source .bash_profile
```

检查

```shell
which tiup
```

#### 2.3 安装 TiUP cluster 组件

```sh
tiup cluster
```

如果已经安装，则更新 TiUP cluster 组件至最新版本：

```sh
tiup update --self && tiup update cluster
```

检查

```sh
tiup --binary cluster
```



### 3. 设置集群拓扑并部署

#### 3.1 集群拓扑设置

>  需要结合具体系统环境与业务需求进行配置, 为了方便功能测试开发，这里提供一个经过测试的单机三节点配置文件，修改对应用户后可以直接部署。

[TIKV_Deploy_Assets/topology.yaml](./TIKV_Deploy_Assets/topology.yaml)



#### 3.2 检查集群存在的潜在风险

> -- user参数需要与拓扑配置文件中保持一致，并保证不同节点间的ssh连接。

```sh
tiup cluster check ./topology.yaml --user root [-p] [-i /home/root/.ssh/gcp_rsa]
```

自动修复集群存在的潜在风险

```shell
tiup cluster check ./topology.yaml --apply --user root [-p] [-i /home/root/.ssh/gcp_rsa]
```



#### 3.3 部署 TiDB 集群

```shell
tiup cluster deploy tidb-test v5.4.0 ./topology.yaml --user root [-p] [-i /home/root/.ssh/gcp_rsa]
```

> - `tidb-test` 为部署的集群名称。
> - `v5.4.0` 为部署的集群版本，可以通过执行 `tiup list tidb` 来查看 TiUP 支持的最新可用版本，推荐使用v5.4.0。

执行如下命令检查 `tidb-test` 集群情况：

```shell
tiup cluster display tidb-test
```

启动集群

```shell
tiup cluster start tidb-test
```

#### 3.4 记录初始密码

安装完成提示初始密码，注意保管，后面调用SQL接口会用到。提示信息类似下方所示

```shell
+ [ Serial ] - UpdateTopology: cluster=rawkv_cluster
Started cluster `rawkv_cluster` successfully
The root password of TiDB database has been changed.
The new password is: 'xxxxxxxxxxx'.
Copy and record it to somewhere safe, it is only displayed once, and will not be stored.
The generated password can NOT be get and shown again.
```
