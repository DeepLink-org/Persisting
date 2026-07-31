import pandas as pd
import pyarrow as pa
from lancedb import LanceDBConnection
from typing import Union, Dict, List, Optional
from lancedb.index import IndexConfig
from dldb.utils import schema_from_string, schema_to_string, filter_values, stable_hash
import threading
from datetime import timedelta


class InformationSchemaRecord:
    def __init__(self, record: dict) -> None:
        self.table_name = record["table_name"]
        self.schema_str = record["schema_str"]
        self.partition_column = record["partition_column"]
        self.partition_type = record["partition_type"]
        self.partitions = record["partitions"]

    @property
    def schema(self):
        return schema_from_string(self.schema_str)


class InformationSchemaTable:
    table_name = "information_schema"

    def __init__(self, db_conn: LanceDBConnection):
        table_names = db_conn.list_tables().tables
        self.db_conn = db_conn
        if self.table_name in table_names:
            self.table = self.db_conn.open_table(self.table_name)
        else:
            self.table = self.db_conn.create_table(
                self.table_name,
                schema=pa.schema(
                    [
                        pa.field("table_name", pa.string()),
                        pa.field("schema_str", pa.string()),
                        pa.field("partition_column", pa.string()),
                        pa.field("partition_type", pa.string()),
                        pa.field("partitions", pa.int32()),
                    ]
                ),
            )
        self.load()

    def load(self):
        records = self.table.to_pandas().to_dict("records")
        self.schema_records = dict()
        for record in records:
            schema_record = InformationSchemaRecord(record)
            self.schema_records[schema_record.table_name] = schema_record

    def add(self, table_name: str, schema: pa.Schema, partition_column: str = None, partition_type: str = None, partitions: int = None):
        schema_str = schema_to_string(schema)
        record = {
            "table_name": table_name,
            "schema_str": schema_str,
            "partition_column": partition_column or "",
            "partition_type": partition_type or "",
            "partitions": partitions or -1,
        }
        if self.exist(table_name):
            self.table.update(where=f"table_name = '{table_name}'", values=record)
        else:
            self.table.add([record])
        self.schema_records[table_name] = InformationSchemaRecord(record)

    def drop(self, table_name: str):
        self.table.delete(f"table_name = '{table_name}'")
        self.schema_records.pop(table_name, None)

    def update_schema(self, table_name: str, schema: pa.Schema):
        schema_str = schema_to_string(schema)
        record = {
            "schema_str": schema_str,
        }
        self.table.update(where=f"table_name = '{table_name}'", values=record)
        self.schema_records[table_name].schema_str = schema_str

    def get(self, table_name: str):
        return self.schema_records.get(table_name, None)

    def exist(self, table_name: str):
        return table_name in self.schema_records

    def reload(self):
        self.table.checkout_latest()
        self.load()

    def list_tables(self):
        return list(self.schema_records.keys())


def _filter_with_lance(table, query: str, limit: int, columns: List[str], offset: int, order_by: str, ascending: bool) -> pd.DataFrame:
    """Use Lance dataset API to filter with order_by support.

    Lance's to_table natively supports order_by, and limit/offset
    are applied after sorting by the engine.
    """
    from lance.dataset import ColumnOrdering

    ds = table.to_lance()
    kwargs = {"filter": query}
    if columns is not None:
        kwargs["columns"] = columns
    if limit is not None:
        kwargs["limit"] = limit
    if offset is not None:
        kwargs["offset"] = offset
    kwargs["order_by"] = [ColumnOrdering(order_by, ascending=ascending)]
    return ds.to_table(**kwargs).to_pandas()


class BaseTable:
    """Base class for table handling"""

    def __init__(self, db_conn: LanceDBConnection, schema_table: InformationSchemaTable, table_name: str, schema: pa.Schema, mode: str) -> None:
        assert table_name, "table_name cannot be empty"
        assert schema_table, "schema_table cannot be empty"
        assert db_conn is not None, "db_conn cannot be None"

        self.raw_table_name = table_name
        self.db_conn = db_conn
        self.schema_table = schema_table
        self.schema = schema
        self.mode = mode

    def create_table(self):
        raise NotImplementedError

    def drop_table(self, partition=None):
        raise NotImplementedError

    def add(self, datas: pd.DataFrame, partition=None):
        raise NotImplementedError

    def count_rows(self, partition=None) -> int:
        raise NotImplementedError

    def filter(
        self,
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

    def create_scalar_index(self, column: str, partition=None, index_type: str = "BTREE"):
        raise NotImplementedError

    def list_indices(self, partition=None) -> list[IndexConfig]:
        raise NotImplementedError

    def optimize(
        self,
        *,
        partition=None,
        cleanup_older_than: Optional[timedelta] = None,
        delete_unverified: bool = False,
        retrain: bool = False,
    ):
        raise NotImplementedError

    def to_pandas(self, partition=None) -> pd.DataFrame:
        raise NotImplementedError

    def clear(self, partition=None):
        raise NotImplementedError

    def delete(self, where: str, partition=None):
        raise NotImplementedError

    def update(self, where: str, values: dict, partition=None):
        raise NotImplementedError

    def upsert(self, columns: List[str], datas: pd.DataFrame, partition=None):
        raise NotImplementedError

    def add_columns(self, transforms: Union[Dict[str, str], pa.field, List[pa.field], pa.Schema]):
        raise NotImplementedError


class SimpleTable(BaseTable):
    """Simple table without partitions"""

    def __init__(
        self, db_conn: LanceDBConnection, schema_table: InformationSchemaTable, table_name: str, schema: pa.Schema = None, mode: str = None
    ) -> None:
        super().__init__(db_conn, schema_table, table_name, schema, mode)
        self.table_name = table_name
        if schema is not None:
            self.table = self.create_table()
        else:
            self.table = self.open_table()

    @classmethod
    def from_table_name(cls, db_conn: LanceDBConnection, schema_table: InformationSchemaTable, table_name: str):
        return cls(db_conn, schema_table, table_name)

    def create_table(self):
        self.table = self.db_conn.create_table(self.table_name, schema=self.schema, mode=self.mode)
        return self.table

    def open_table(self):
        self.table = self.db_conn.open_table(self.table_name)
        return self.table

    def drop_table(self, partition=None):
        assert partition is None, "Partitioning not supported for SimpleTable"
        self.db_conn.drop_table(self.table_name)

    def add(self, datas: pd.DataFrame, partition=None):
        assert partition is None, "Partitioning not supported for SimpleTable"
        if self.table is None:
            self.open_table()
        self.table.add(datas)

    def count_rows(self, partition=None) -> int:
        assert partition is None, "Partitioning not supported for SimpleTable"
        if self.table is None:
            self.open_table()
        return self.table.count_rows()

    def filter(
        self,
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
        assert partitions is None and partition_cond is None, "Partitioning not supported for SimpleTable"
        if self.table is None:
            self.open_table()
        if checkout_latest:
            self.table.checkout_latest()
        if order_by is not None:
            return _filter_with_lance(self.table, query, limit, columns, offset, order_by, ascending)
        query_stat = self.table.search().where(query)
        if columns is not None:
            query_stat = query_stat.select(columns)
        if limit is not None:
            query_stat = query_stat.limit(limit)
        if offset is not None:
            query_stat = query_stat.offset(offset)
        return query_stat.to_pandas()

    def create_scalar_index(self, column: str, partition=None, index_type: str = "BTREE"):
        assert partition is None, "Partitioning not supported for SimpleTable"
        if self.table is None:
            self.open_table()
        self.table.create_scalar_index(column, index_type=index_type)
        index_name = f"{column}_idx"
        self.table.wait_for_index([index_name])

    def list_indices(self, partition=None) -> list[IndexConfig]:
        assert partition is None, "Partitioning not supported for SimpleTable"
        if self.table is None:
            self.open_table()
        return self.table.list_indices()

    def optimize(
        self,
        *,
        partition=None,
        cleanup_older_than: Optional[timedelta] = None,
        delete_unverified: bool = False,
        retrain: bool = False,
    ):
        assert partition is None, "Partitioning not supported for SimpleTable"
        if self.table is None:
            self.open_table()
        return self.table.optimize(cleanup_older_than=cleanup_older_than, delete_unverified=delete_unverified, retrain=retrain)

    def to_pandas(self, partition=None) -> pd.DataFrame:
        assert partition is None, "Partitioning not supported for SimpleTable"
        if self.table is None:
            self.open_table()
        return self.table.to_pandas()

    def delete(self, where: str, partition=None):
        assert partition is None, "Partitioning not supported for SimpleTable"
        if self.table is None:
            self.open_table()
        self.table.delete(where)

    def update(self, where: str, values: dict, partition=None):
        assert partition is None, "Partitioning not supported for SimpleTable"
        if self.table is None:
            self.open_table()
        self.table.update(where, values)

    def upsert(self, columns: List[str], datas: pd.DataFrame, partition=None):
        assert partition is None, "Partitioning not supported for SimpleTable"
        if self.table is None:
            self.open_table()

        self.table.merge_insert(columns).when_matched_update_all().when_not_matched_insert_all().execute(datas)

    def add_columns(self, transforms: Union[Dict[str, str], pa.field, List[pa.field], pa.Schema]):
        if self.table is None:
            self.open_table()

        self.table.add_columns(transforms)
        self.schema = self.table.schema


class ValuePartitionTable(BaseTable):
    """Table for VALUE partition type"""

    def __init__(
        self,
        db_conn: LanceDBConnection,
        schema_table: InformationSchemaTable,
        table_name: str,
        schema: pa.Schema,
        mode: str,
        partition_column: str,
        partition: str = None,
    ) -> None:
        super().__init__(db_conn, schema_table, table_name, schema, mode)
        assert partition_column, "Partition column must be specified"
        assert mode != "overwrite", "mode cannot be overwrite for VALUE partition type"

        self.partition_column = partition_column
        self.table_name_prefix = f"{table_name}_type_VALUE_column_{partition_column}_partition_"
        self.tables = {}
        self._lock = threading.Lock()  # Add thread lock for concurrent access
        if partition is not None and mode == "OPEN":
            self.open_table([partition], create_when_missing=False)

    def add(self, datas: pd.DataFrame, partition=None):
        dfs = {d: g.reset_index(drop=True) for d, g in datas.groupby(self.partition_column, sort=False)}
        if partition is not None:
            assert len(dfs) == 1 and partition in dfs, f"datas must belong partition='{partition}'"

        with self._lock:
            self.open_table(list(dfs.keys()), create_when_missing=True)
        for partition, value in dfs.items():
            self.tables[partition].add(value)

    def get_table_name(self, partition):
        return self.table_name_prefix + str(partition)

    def get_partition(self, table_name):
        return table_name[len(self.table_name_prefix) :]

    def open_table(self, partitions, create_when_missing=False):
        table_names = set(self.db_conn.list_tables().tables)
        for partition in partitions:
            assert partition, "partition can not be None"
            assert isinstance(partition, str), "partition must be a string"
            if partition in self.tables:
                continue

            table_name = self.get_table_name(partition)
            if table_name in table_names:
                table = self.db_conn.open_table(table_name)
            elif create_when_missing:
                try:
                    table = self.db_conn.create_table(
                        table_name,
                        schema=self.schema,
                        mode="create",
                    )
                except ValueError as e:
                    # Table might have been created by another thread
                    if "already exists" in str(e):
                        table = self.db_conn.open_table(table_name)
                    else:
                        raise
            else:
                raise ValueError(f"Table {self.raw_table_name} partition {partition} does not exist")
            self.tables[partition] = table

    def list_partitions(self) -> list[str]:
        table_names = set(self.db_conn.list_tables().tables)
        partitions = []
        for table_name in table_names:
            if table_name.startswith(self.table_name_prefix):
                partitions.append(self.get_partition(table_name))
        return partitions

    def count_rows(self, partition=None) -> int:
        if partition is not None:
            self.open_table([partition])
            return self.tables[partition].count_rows()

        all_partitions = self.list_partitions()
        self.open_table(all_partitions)
        result = 0
        for table in self.tables.values():
            result += table.count_rows()
        return result

    def _contain_index(self, table, index_name):
        index_list = table.list_indices()
        for index in index_list:
            if index.name == index_name:
                return True
        return False

    def create_scalar_index(self, column: str, partition=None, index_type: str = "BTREE"):
        if partition is not None:
            partitions = [partition]
        else:
            partitions = self.list_partitions()

        self.open_table(partitions)
        for partition in partitions:
            index_name = f"{column}_idx"
            self.tables[partition].create_scalar_index(column, index_type=index_type)
            self.tables[partition].wait_for_index([index_name])

    def list_indices(self, partition) -> list[IndexConfig]:
        assert partition is not None, "partition cannot be None when value partition table listing indices"
        self.open_table([partition])
        return self.tables[partition].list_indices()

    def optimize(
        self,
        *,
        partition=None,
        cleanup_older_than: Optional[timedelta] = None,
        delete_unverified: bool = False,
        retrain: bool = False,
    ):
        if partition is not None:
            partitions = [partition]
        else:
            partitions = self.list_partitions()

        self.open_table(partitions)
        for p in partitions:
            self.tables[p].optimize(cleanup_older_than=cleanup_older_than, delete_unverified=delete_unverified, retrain=retrain)

    def drop_table(self, partition=None):
        if partition is not None:
            partitions = [partition]
        else:
            partitions = self.list_partitions()

        for partition in partitions:
            table_name = self.get_table_name(partition)
            self.db_conn.drop_table(table_name)
            self.tables.pop(partition, None)

    def filter(
        self,
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
        if partitions is None:
            partitions = self.list_partitions()
            if partition_cond is not None:
                partitions = filter_values(partitions, partition_cond, column_name=self.partition_column)
        if offset is not None:
            assert len(partitions) == 1, "offset is not supported for multiple partitions"
        partitions = sorted(partitions)

        result = pd.DataFrame()
        for partition in partitions:
            self.open_table([partition])
            table = self.tables[partition]
            if checkout_latest:
                table.checkout_latest()
            if order_by is not None:
                query_result = _filter_with_lance(table, query, limit, columns, offset, order_by, ascending)
            else:
                query_stat = table.search().where(query)
                if columns is not None:
                    query_stat = query_stat.select(columns)
                if limit is not None:
                    query_stat = query_stat.limit(limit)
                if offset is not None:
                    query_stat = query_stat.offset(offset)
                query_result = query_stat.to_pandas()
            result = pd.concat([result, query_result], ignore_index=True)
            if limit is not None:
                limit -= len(query_result)
                if limit <= 0:
                    break

        return result

    def to_pandas(self, partition) -> pd.DataFrame:
        assert partition is not None, "partition cannot be None when value partition table to_pandas"
        self.open_table([partition])
        return self.tables[partition].to_pandas()

    def delete(self, where: str, partition=None):
        partitions = []
        if partition is not None:
            partitions = [partition]
        else:
            partitions = self.list_partitions()

        self.open_table(partitions)
        for partition in partitions:
            self.tables[partition].delete(where)

    def update(self, where: str, values: dict, partition=None):
        partitions = []
        if partition is not None:
            partitions = [partition]
        else:
            partitions = self.list_partitions()

        self.open_table(partitions)
        for partition in partitions:
            self.tables[partition].update(where, values)

    def upsert(self, columns: List[str], datas: pd.DataFrame, partition=None):
        dfs = {d: g.reset_index(drop=True) for d, g in datas.groupby(self.partition_column, sort=False)}
        if partition is not None:
            assert len(dfs) == 1 and partition in dfs, f"datas must belong partition='{partition}'"

        self.open_table(list(dfs.keys()), create_when_missing=True)
        for partition, value in dfs.items():
            self.tables[partition].merge_insert(columns).when_matched_update_all().when_not_matched_insert_all().execute(value)

    @classmethod
    def from_table_name(cls, db_conn: LanceDBConnection, schema_table: InformationSchemaTable, table_name: str, partition=None):
        if not schema_table.exist(table_name):
            schema_table.reload()
        assert schema_table.exist(table_name), f"Table {table_name} does not exist"
        schema_record = schema_table.get(table_name)
        assert schema_record.schema is not None, f"Table {table_name} does not have a schema"
        assert schema_record.partition_column is not None, f"Table {table_name} does not have a partition_column"
        assert schema_record.partition_type == "VALUE", f"Table {table_name} partition_type={schema_record.partition_type}, not equal to VALUE"

        return cls(
            db_conn,
            schema_table,
            table_name,
            schema=schema_record.schema,
            mode="OPEN",
            partition_column=schema_record.partition_column,
            partition=partition,
        )

    def add_columns(self, transforms: Union[Dict[str, str], pa.field, List[pa.field], pa.Schema]):
        partitions = self.list_partitions()
        self.open_table(partitions)
        schema_updated = False
        for partition in partitions:
            self.tables[partition].add_columns(transforms)
            if not schema_updated:
                self.schema = self.tables[partition].schema
                schema_updated = True


class HashPartitionTable(BaseTable):
    """Table for HASH partition type"""

    def __init__(
        self,
        db_conn: LanceDBConnection,
        schema_table: InformationSchemaTable,
        table_name: str,
        schema: pa.Schema,
        mode: str,
        partition_column: str,
        partitions: int,
        partition: int = None,
    ) -> None:
        super().__init__(db_conn, schema_table, table_name, schema, mode)
        assert isinstance(partitions, int) and partitions > 0, "partitions must be a positive integer"
        assert partition_column, "Partition column must be specified"
        assert mode != "overwrite", "mode cannot be overwrite for HASH partition type"

        self.partition_column = partition_column
        self.partitions = partitions
        self.table_name_prefix = f"{table_name}_type_HASH_column_{partition_column}_partitions_{partitions}_partition_"
        self.tables = {}
        self._lock = threading.Lock()  # Add thread lock for concurrent access
        if partition is not None and mode == "OPEN":
            self.open_table([partition], create_when_missing=False)

    def _hash_partition(self, value) -> int:
        """Calculate hash partition index for a value"""
        return stable_hash(value) % self.partitions

    def get_table_name(self, partition: int) -> str:
        return self.table_name_prefix + str(partition)

    def get_partition(self, table_name: str) -> int:
        return int(table_name[len(self.table_name_prefix) :])

    def open_table(self, partitions: List[int], create_when_missing=False):
        table_names = set(self.db_conn.list_tables().tables)
        for partition in partitions:
            assert partition is not None, "partition can not be None"
            assert isinstance(partition, int), "partition must be an integer"
            assert 0 <= partition < self.partitions, f"partition must be in range [0, {self.partitions})"
            if partition in self.tables:
                continue

            table_name = self.get_table_name(partition)
            if table_name in table_names:
                table = self.db_conn.open_table(table_name)
            elif create_when_missing:
                try:
                    table = self.db_conn.create_table(
                        table_name,
                        schema=self.schema,
                        mode="create",
                    )
                except ValueError as e:
                    # Table might have been created by another thread
                    if "already exists" in str(e):
                        table = self.db_conn.open_table(table_name)
                    else:
                        raise
            else:
                raise ValueError(f"Table {self.raw_table_name} partition {partition} does not exist")
            self.tables[partition] = table

    def list_partitions(self) -> List[int]:
        table_names = set(self.db_conn.list_tables().tables)
        partitions = []
        for table_name in table_names:
            if table_name.startswith(self.table_name_prefix):
                partitions.append(self.get_partition(table_name))
        return partitions

    def add(self, datas: pd.DataFrame, partition=None):
        # Group data by hash partition
        datas = datas.copy()
        datas["_hash_partition"] = datas[self.partition_column].apply(self._hash_partition)
        dfs = {d: g.drop(columns=["_hash_partition"]).reset_index(drop=True) for d, g in datas.groupby("_hash_partition", sort=False)}

        if partition is not None:
            assert len(dfs) == 1 and partition in dfs, f"datas must belong partition={partition}"

        with self._lock:
            self.open_table(list(dfs.keys()), create_when_missing=True)
        for partition_idx, value in dfs.items():
            self.tables[partition_idx].add(value)

    def count_rows(self, partition=None) -> int:
        if partition is not None:
            self.open_table([partition])
            return self.tables[partition].count_rows()

        all_partitions = self.list_partitions()
        self.open_table(all_partitions)
        result = 0
        for table in self.tables.values():
            result += table.count_rows()
        return result

    def _contain_index(self, table, index_name):
        index_list = table.list_indices()
        for index in index_list:
            if index.name == index_name:
                return True
        return False

    def create_scalar_index(self, column: str, partition=None, index_type: str = "BTREE"):
        if partition is not None:
            partitions = [partition]
        else:
            partitions = self.list_partitions()

        self.open_table(partitions)
        for partition in partitions:
            index_name = f"{column}_idx"
            self.tables[partition].create_scalar_index(column, index_type=index_type)
            self.tables[partition].wait_for_index([index_name])

    def list_indices(self, partition) -> list[IndexConfig]:
        assert partition is not None, "partition cannot be None when hash partition table listing indices"
        self.open_table([partition])
        return self.tables[partition].list_indices()

    def optimize(
        self,
        *,
        partition=None,
        cleanup_older_than: Optional[timedelta] = None,
        delete_unverified: bool = False,
        retrain: bool = False,
    ):
        if partition is not None:
            partitions = [partition]
        else:
            partitions = self.list_partitions()

        self.open_table(partitions)
        for p in partitions:
            self.tables[p].optimize(cleanup_older_than=cleanup_older_than, delete_unverified=delete_unverified, retrain=retrain)

    def drop_table(self, partition=None):
        if partition is not None:
            partitions = [partition]
        else:
            partitions = self.list_partitions()

        for partition in partitions:
            table_name = self.get_table_name(partition)
            self.db_conn.drop_table(table_name)
            self.tables.pop(partition, None)

    def filter(
        self,
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
        assert partition_cond is None, "partition_cond is not supported for hash partition table"

        if partitions is None:
            partitions = self.list_partitions()
        if offset is not None:
            assert len(partitions) == 1, "offset is not supported for multiple partitions"
        partitions = sorted(partitions)

        result = pd.DataFrame()
        for partition in partitions:
            self.open_table([partition])
            table = self.tables[partition]
            if checkout_latest:
                table.checkout_latest()
            if order_by is not None:
                query_result = _filter_with_lance(table, query, limit, columns, offset, order_by, ascending)
            else:
                query_stat = table.search().where(query)
                if columns is not None:
                    query_stat = query_stat.select(columns)
                if limit is not None:
                    query_stat = query_stat.limit(limit)
                if offset is not None:
                    query_stat = query_stat.offset(offset)
                query_result = query_stat.to_pandas()
            result = pd.concat([result, query_result], ignore_index=True)
            if limit is not None:
                limit -= len(query_result)
                if limit <= 0:
                    break

        return result

    def to_pandas(self, partition) -> pd.DataFrame:
        assert partition is not None, "partition cannot be None when hash partition table to_pandas"
        self.open_table([partition])
        return self.tables[partition].to_pandas()

    def delete(self, where: str, partition=None):
        partitions = []
        if partition is not None:
            partitions = [partition]
        else:
            partitions = self.list_partitions()

        self.open_table(partitions)
        for partition in partitions:
            self.tables[partition].delete(where)

    def update(self, where: str, values: dict, partition=None):
        partitions = []
        if partition is not None:
            partitions = [partition]
        else:
            partitions = self.list_partitions()

        self.open_table(partitions)
        for partition in partitions:
            self.tables[partition].update(where, values)

    def upsert(self, columns: List[str], datas: pd.DataFrame, partition=None):
        # Group data by hash partition
        datas = datas.copy()
        datas["_hash_partition"] = datas[self.partition_column].apply(self._hash_partition)
        dfs = {d: g.drop(columns=["_hash_partition"]).reset_index(drop=True) for d, g in datas.groupby("_hash_partition", sort=False)}

        if partition is not None:
            assert len(dfs) == 1 and partition in dfs, f"datas must belong partition={partition}"

        self.open_table(list(dfs.keys()), create_when_missing=True)
        for partition_idx, value in dfs.items():
            self.tables[partition_idx].merge_insert(columns).when_matched_update_all().when_not_matched_insert_all().execute(value)

    @classmethod
    def from_table_name(cls, db_conn: LanceDBConnection, schema_table: InformationSchemaTable, table_name: str, partition=None):
        if not schema_table.exist(table_name):
            schema_table.reload()
        assert schema_table.exist(table_name), f"Table {table_name} does not exist"
        schema_record = schema_table.get(table_name)
        assert schema_record.schema is not None, f"Table {table_name} does not have a schema"
        assert schema_record.partition_column is not None, f"Table {table_name} does not have a partition_column"
        assert schema_record.partition_type == "HASH", f"Table {table_name} partition_type={schema_record.partition_type}, not equal to HASH"
        assert schema_record.partitions > 0, f"Table {table_name} does not have valid partitions count"

        return cls(
            db_conn,
            schema_table,
            table_name,
            schema=schema_record.schema,
            mode="OPEN",
            partition_column=schema_record.partition_column,
            partitions=schema_record.partitions,
            partition=partition,
        )

    def add_columns(self, transforms: Union[Dict[str, str], pa.field, List[pa.field], pa.Schema]):
        partitions = self.list_partitions()
        self.open_table(partitions)
        schema_updated = False
        for partition in partitions:
            self.tables[partition].add_columns(transforms)
            if not schema_updated:
                self.schema = self.tables[partition].schema
                schema_updated = True


def create_table(
    db_conn: LanceDBConnection,
    schema_table: InformationSchemaTable,
    table_name: str,
    schema: pa.Schema,
    mode: str,
    partition_column: str = None,
    partition_type: str = None,
    partitions: int = None,
) -> BaseTable:
    if partition_column is None:
        return SimpleTable(db_conn, schema_table, table_name, schema, mode)

    if partition_type == "VALUE":
        return ValuePartitionTable(db_conn, schema_table, table_name, schema, mode, partition_column)
    elif partition_type == "HASH":
        return HashPartitionTable(db_conn, schema_table, table_name, schema, mode, partition_column, partitions)
    else:
        raise ValueError(f"Partition type must be either 'HASH' or 'VALUE', got '{partition_type}'")


def open_table(db_conn: LanceDBConnection, schema_table: InformationSchemaTable, table_name: str, full_table_name: str, partition=None) -> BaseTable:
    if "_type_VALUE_" in full_table_name:
        return ValuePartitionTable.from_table_name(db_conn, schema_table, table_name, partition)
    if "_type_HASH_" in full_table_name:
        return HashPartitionTable.from_table_name(db_conn, schema_table, table_name, partition)

    assert table_name == full_table_name
    return SimpleTable.from_table_name(db_conn, schema_table, table_name)


def open_table_by_partition_type(
    db_conn: LanceDBConnection, schema_table: InformationSchemaTable, table_name: str, partition_type: str, partition=None
) -> BaseTable:
    if partition_type == "VALUE":
        return ValuePartitionTable.from_table_name(db_conn, schema_table, table_name, partition)
    if partition_type == "HASH":
        return HashPartitionTable.from_table_name(db_conn, schema_table, table_name, partition)

    return SimpleTable.from_table_name(db_conn, schema_table, table_name)
