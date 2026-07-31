import pandas as pd
from dataclasses import dataclass
from collections import deque
import lancedb
from loguru import logger
import sys
import pyarrow as pa
from typing import Union, Dict, List, Optional, Any
from lancedb.index import IndexConfig
from datetime import timedelta

from dldb.table import create_table, open_table, open_table_by_partition_type, InformationSchemaTable
from dldb.metrics import MetricsCollector

logger.remove()
logger.add(sys.stdout, level="INFO")


@dataclass
class Config:
    use_memory_queue: bool
    flush_every: int
    storage_options: dict
    model: Optional[str]
    last_calls_maxlen: int

    @staticmethod
    def from_args(
        use_memory_queue: bool = False,
        flush_every: int = 1000,
        storage_options: dict = None,
        model: Optional[str] = None,
        last_calls_maxlen: int = 100,
        **kwargs,
    ):
        _ = kwargs
        return Config(
            use_memory_queue=use_memory_queue,
            flush_every=flush_every,
            storage_options=storage_options,
            model=model if model != "" else None,
            last_calls_maxlen=int(last_calls_maxlen),
        )


class SessionBase:
    def __init__(self, **kwargs) -> None:
        super().__init__()
        self.config = Config.from_args(**kwargs)

    def shutdown(self) -> None:
        pass

    def create_table(
        self,
        table_name: str,
        schema: pa.Schema,
        mode: str = "create",
        partition_column: str = None,
        partition_type: str = "VALUE",
        partitions: int = -1,
    ):
        """Create table in the database.

        Parameters
        ----------
        table_name: The name of the table
        schema: The schema of the table
        mode: str; default "create"
            The mode to use when creating the table.
            Can be either "create" or "overwrite".
            By default, if the table already exists, an exception is raised.
            If you want to overwrite the table, use mode="overwrite".
        partition_column: str; partition column, default None
        partition_type: str; default "VALUE"
            The type to use when partitioning the table.
            Can be either "VALUE" or "HASH".
        partitions: int; number of partitions, default -1.
            If partition_type is "VALUE", partitions will be ignore and should always be inf.
            If partition_type is "HASH", partitions must be greater than zero and will be used to determine the number of partitions.

        Examples
        --------
        >>> import pyarrow as pa
        >>> custom_schema = pa.schema([
        ...   pa.field("vector", pa.list_(pa.float32(), 2)),
        ...   pa.field("lat", pa.float32()),
        ...   pa.field("long", pa.float32())
        ... ])
        >>> session.create("table", schema = custom_schema)
        """
        raise NotImplementedError

    def add(self, table_name: str, datas: pd.DataFrame, partition=None):
        raise NotImplementedError

    def count_rows(self, table_name: str, partition=None):
        raise NotImplementedError

    def filter(
        self,
        table_name: str,
        query: str,
        limit: int = None,
        columns: List[str] = None,
        offset: int = None,
        *,
        partitions: list = None,
        partition_cond: str = None,
        order_by: str = None,
        ascending: bool = True,
        checkout_latest: bool = False,
    ) -> pd.DataFrame:
        raise NotImplementedError

    def create_scalar_index(self, table_name: str, column: str, *, partition=None, index_type: str = "BTREE"):
        raise NotImplementedError

    def list_indices(self, table_name: str, partition=None) -> list[IndexConfig]:
        raise NotImplementedError

    def optimize(
        self,
        table_name: str,
        *,
        partition=None,
        cleanup_older_than: Optional[timedelta] = None,
        delete_unverified: bool = False,
        retrain: bool = False,
    ):
        raise NotImplementedError

    def drop_table(self, table_name: str, partition=None):
        raise NotImplementedError

    def table_exists(self, table_name: str) -> bool:
        raise NotImplementedError

    def list_tables(self) -> List[str]:
        raise NotImplementedError

    def get_schema(self, table_name: str) -> pa.Schema:
        raise NotImplementedError

    def delete(self, table_name: str, where: str, partition=None):
        raise NotImplementedError

    def update(self, table_name: str, where: str, values: dict, partition=None):
        raise NotImplementedError

    def upsert(self, table_name: str, columns: List[str], datas: pd.DataFrame, partition=None):
        """When data not exist in table, insert data, otherwise update data.

        Parameters
        ----------

        columns: List[str]
        columns to join on.  This is how records from the
        source table and target table are matched.

        Examples
        --------
        >>> datas = pd.DataFrame({"id": [1, 2, 3], "name": ["a", "b", "c"], "date": ["2025", "2026", "2027"]})
        >>> session.upsert("table", columns=["id", "name"], datas=datas, partition="2025")
        """
        raise NotImplementedError

    def add_columns(self, table_name: str, transforms: Union[Dict[str, str], pa.field, List[pa.field], pa.Schema]):
        """
        Add new columns with defined values.

        Parameters
        ----------
        transforms: Dict[str, str], pa.Field, List[pa.Field], pa.Schema
            A map of column name to a SQL expression to use to calculate the
            value of the new column. These expressions will be evaluated for
            each row in the table, and can reference existing columns.
            Alternatively, a pyarrow Field or Schema can be provided to add
            new columns with the specified data types. The new columns will
            be initialized with null values.

        Examples
        --------
        >>> data = pa.table({"id": [0, 1]})
        >>> table = db.create_table("my_table", data=data)
        >>> table.add_columns([pa.field("x", pa.int64()), pa.field("vector", pa.list_(pa.float32(), 8))])   # table will have columns: id, x, vector
        >>> table.add_columns(pa.schema([pa.field("y", pa.int64()), pa.field("emb", pa.list_(pa.float32(), 8))]))   # table will have columns: id, x, vector, y, emb
        >>> table.add_columns({"new_col": "id + 2"})    # table will have columns: id, x, vector, y, emb, new_col
        """
        raise NotImplementedError

    def schema(self, table_name: str):
        raise NotImplementedError


class DBName:
    def __init__(self, db_name: str) -> None:
        if not isinstance(db_name, str):
            raise TypeError("db_name must be a string")
        if not db_name:
            raise ValueError("db_name cannot be empty")
        self.db_name = db_name

    @property
    def memory_db_name(self):
        return f"memory://{self.db_name}"


class LanceSession(SessionBase):
    def __init__(self, db_name: str, **kwargs) -> None:
        super().__init__(**kwargs)
        self.db_name = DBName(db_name)
        self.db_conn = lancedb.connect(db_name, storage_options=self.config.storage_options)
        self.schema_table = InformationSchemaTable(self.db_conn)
        self.tables = dict()
        self.last_call: Optional[dict] = None
        self.last_calls = deque(maxlen=self.config.last_calls_maxlen)
        self._metrics: Optional[MetricsCollector] = MetricsCollector() if self.config.model == "metrics" else None
        if self.config.use_memory_queue:
            self.memory_db_conn = lancedb.connect(self.db_name.memory_db_name)
            self.memory_tables = dict()

    def _maybe_df_bytes(self, df: Any) -> Optional[int]:
        try:
            if isinstance(df, pd.DataFrame):
                return int(df.memory_usage(deep=True).sum())
        except Exception:
            return None
        return None

    def _rows_bytes_from_result(self, api: str, result: Any, *, datas: Optional[pd.DataFrame] = None) -> tuple[Optional[int], Optional[int]]:
        if api == "add" and datas is not None:
            rows = int(len(datas))
            bytes_ = self._maybe_df_bytes(datas)
            return rows, bytes_
        if isinstance(result, pd.DataFrame):
            rows = int(len(result))
            bytes_ = self._maybe_df_bytes(result)
            return rows, bytes_
        return None, None

    def _record_call(self, timing: dict) -> None:
        self.last_call = timing
        self.last_calls.append(timing)
        if self._metrics is not None:
            self._metrics.record(timing)

    def create_table(
        self,
        table_name: str,
        schema: pa.Schema,
        mode: str = "create",
        partition_column: str = None,
        partition_type: str = "VALUE",
        partitions: int = -1,
    ):
        assert table_name, "Table name cannot be empty"
        assert schema, "Schema cannot be None"
        assert mode in ["create", "overwrite"], "Mode must be one of 'create', 'overwrite'"
        if self.schema_table.exist(table_name):
            if mode == "create":
                raise ValueError(f"Table '{table_name}' already exists")

        table = create_table(
            self.db_conn,
            self.schema_table,
            table_name,
            schema,
            mode,
            partition_column,
            partition_type,
            partitions,
        )
        self.tables[table.raw_table_name] = table
        self.schema_table.add(table_name, schema, partition_column, partition_type, partitions)
        if self.config.use_memory_queue:
            memory_table = create_table(
                self.memory_db_conn,
                self.schema_table,
                table_name,
                schema,
                mode,
                partition_column,
                partition_type,
                partitions,
            )
            self.memory_tables[memory_table.raw_table_name] = memory_table

    def _open_disk_table(self, table_name: str, partition=None):
        table_names = self.db_conn.list_tables().tables
        for full_table_name in table_names:
            if full_table_name.startswith(table_name):
                return open_table(self.db_conn, self.schema_table, table_name, full_table_name, partition)
        return None

    def _get_table(self, table_name: str, partition=None):
        table = self.tables.get(table_name, None)
        if table is not None:
            return table

        table = self._open_disk_table(table_name, partition)
        if table is None and self.schema_table.exist(table_name):
            record = self.schema_table.get(table_name)
            table = open_table_by_partition_type(self.db_conn, self.schema_table, table_name, record.partition_type, partition)
        assert table, f"{table_name} not exist"
        self.tables[table_name] = table
        return table

    def _add_to_disk(self, table_name: str, datas: pd.DataFrame, partition=None):
        table = self._get_table(table_name, partition)
        table.add(datas, partition)
        logger.debug(f"add {len(datas)} rows to {table_name}")

    def _add_to_memory(self, table_name: str, datas: pd.DataFrame, partition=None):
        table = self.memory_tables(table_name, None)
        if len(datas) >= self.config.flush_every or (table is not None and (table.count_rows(partition) + len(datas) >= self.config.flush_every)):
            if table is not None:
                datas = pd.concat([table.to_pandas(partition), datas], ignore_index=True)
                table.clear(partition)
            self._add_to_disk(table_name, datas, partition)
            return

        if table is None:
            table = self.memory_db_conn.create_table(table_name, datas)
            self.memory_tables[table_name] = table
        else:
            table.add(datas, partition)
        logger.debug(f"add {len(datas)} rows to {table_name}")

    def add(self, table_name: str, datas: pd.DataFrame, partition=None):
        if not self.config.use_memory_queue:
            return self._add_to_disk(table_name, datas, partition)
        return self._add_to_memory(table_name, datas, partition)

    def count_rows(self, table_name: str, partition=None):
        table = self._get_table(table_name, partition)
        return table.count_rows(partition)

    def filter(
        self,
        table_name: str,
        query: str,
        limit: int = None,
        columns: List[str] = None,
        offset: int = None,
        *,
        partitions: list = None,
        partition_cond: str = None,
        order_by: str = None,
        ascending: bool = True,
        checkout_latest: bool = False,
    ) -> pd.DataFrame:
        if partitions is not None and len(partitions) > 0:
            assert partition_cond is None, "partition_cond is not supported with partitions"
        table = self._get_table(table_name, None)
        return table.filter(
            query,
            limit,
            columns,
            offset=offset,
            partitions=partitions,
            partition_cond=partition_cond,
            order_by=order_by,
            ascending=ascending,
            checkout_latest=checkout_latest,
        )

    def create_scalar_index(self, table_name: str, column: str, *, partition=None, index_type: str = "BTREE"):
        table = self._get_table(table_name, partition)
        return table.create_scalar_index(column, partition, index_type)

    def list_indices(self, table_name: str, partition=None) -> list[IndexConfig]:
        table = self._get_table(table_name, partition)
        return table.list_indices(partition)

    def optimize(
        self,
        table_name: str,
        *,
        partition=None,
        cleanup_older_than: Optional[timedelta] = None,
        delete_unverified: bool = False,
        retrain: bool = False,
    ):
        table = self._get_table(table_name, partition)
        return table.optimize(partition=partition, cleanup_older_than=cleanup_older_than, delete_unverified=delete_unverified, retrain=retrain)

    def drop_table(self, table_name: str, partition=None):
        table = self._get_table(table_name, partition)
        table.drop_table(partition)
        if partition is None:
            self.tables.pop(table_name, None)
            self.schema_table.drop(table_name)

    def _full_flush(self):
        memory_table_to_deletes = []
        disk_table_to_adds = []
        for db_name, dbinfo in self.dbs.items():
            if not self._is_memory_db(db_name):
                continue

            disk_db_name = self._memory_db_to_disk_db(db_name)
            for table_name, table in dbinfo.tables.items():
                disk_table_to_adds.append((disk_db_name, table_name, table.to_pandas()))
                memory_table_to_deletes.append((db_name, table_name))

        for db_name, table_name, datas in disk_table_to_adds:
            self._add_to_disk(db_name, table_name, datas)

        for db_name, table_name in memory_table_to_deletes:
            self._delete_table(db_name, table_name)

    def shutdown(self):
        if self.config.use_memory_queue:
            self._full_flush()
        _ = super().shutdown()
        if self.config.model == "metrics" and self._metrics is not None:
            return self._metrics.summary()
        return None

    def table_exists(self, table_name: str) -> bool:
        return self.schema_table.exist(table_name)

    def list_tables(self) -> List[str]:
        return self.schema_table.list_tables()

    def get_schema(self, table_name: str) -> pa.Schema:
        record = self.schema_table.get(table_name)
        if record is None:
            raise ValueError(f"Table '{table_name}' not exist")
        return record.schema

    def delete(self, table_name: str, where: str, partition=None):
        assert table_name, "table_name is required"
        assert where, "where is required"
        table = self._get_table(table_name, partition)
        return table.delete(where, partition)

    def update(self, table_name: str, where: str, values: dict, partition=None):
        assert table_name, "table_name is required"
        assert where, "where is required"
        assert values, "values is required"
        table = self._get_table(table_name, partition)
        return table.update(where, values, partition)

    def upsert(self, table_name: str, columns: List[str], datas: pd.DataFrame, partition=None):
        assert table_name, "table_name is required"
        assert columns and len(columns) > 0, "columns is required"
        assert datas is not None and len(datas) > 0, "datas is required"
        table = self._get_table(table_name, partition)
        return table.upsert(columns, datas, partition)

    def add_columns(self, table_name: str, transforms: Union[Dict[str, str], pa.field, List[pa.field], pa.Schema]):
        assert table_name, "table_name is required"
        assert transforms is not None, "transforms is required"
        table = self._get_table(table_name)
        table.add_columns(transforms)
        self.schema_table.update_schema(table_name, table.schema)

    def schema(self, table_name: str):
        table = self._get_table(table_name)
        return table.schema
