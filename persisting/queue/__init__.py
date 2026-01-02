"""Persisting Queue - Lance 持久化后端

为 Pulsing 队列系统提供持久化存储后端：
- LanceBackend: 基础 Lance 持久化
- PersistingBackend: 增强版（WAL、监控指标等）

使用方式：
    from pulsing.queue import write_queue, register_backend
    from persisting.queue import LanceBackend, PersistingBackend
    
    # 方式 1：注册后使用名称
    register_backend("lance", LanceBackend)
    writer = await write_queue(system, "topic", backend="lance")
    
    # 方式 2：直接传类
    writer = await write_queue(system, "topic", backend=LanceBackend)
    
    # 使用增强版后端
    register_backend("persisting", PersistingBackend)
    writer = await write_queue(
        system, "topic", 
        backend="persisting",
        backend_options={"enable_wal": True, "enable_metrics": True}
    )
"""

from .backend import LanceBackend, PersistingBackend

__all__ = ["LanceBackend", "PersistingBackend"]
