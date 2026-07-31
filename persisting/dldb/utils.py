import hashlib
from typing import Union
import base64
import io
import pyarrow as pa
import pyarrow.ipc as ipc
import sqlglot
from sqlglot import exp
from typing import List


def stable_hash(value: Union[int, str]) -> int:
    """Stable, cross-Python-version hash for int/str.

    Rules:
    - int  -> return itself (unchanged)
    - str  -> deterministic hash -> int (stable across Python versions/platforms)
    - else -> TypeError
    """
    if isinstance(value, int):
        return int(value)

    if isinstance(value, str):
        digest = hashlib.blake2b(value.encode("utf-8"), digest_size=8).digest()
        return int.from_bytes(digest, byteorder="big", signed=False)

    raise TypeError(f"Unsupported type: {type(value).__name__} (only int or str are allowed)")


def schema_to_string(schema: pa.Schema) -> str:
    sink = io.BytesIO()
    with ipc.new_stream(sink, schema):
        pass
    return base64.b64encode(sink.getvalue()).decode("utf-8")


def schema_from_string(s: str) -> pa.Schema:
    data = base64.b64decode(s.encode("utf-8"))
    source = io.BytesIO(data)
    reader = ipc.open_stream(source)
    return reader.schema


class PartitionFilter:
    """Filter data list using SQL parser library"""

    def __init__(self, values: List[str], column_name: str = "partition"):
        """
        Initialize the filter

        Args:
            values: List of all available values
            column_name: Column name, default is 'partition'
        """
        self.values = values
        self.column_name = column_name

    def filter(self, condition: str) -> List[str]:
        """
        Filter data list based on SQL condition statement

        Args:
            condition: SQL WHERE condition statement, e.g. "partition = '20260101'" or "date = '20260101'"

        Returns:
            List of values that meet the condition
        """
        # Build complete SQL statement
        sql = f"SELECT * FROM t WHERE {condition}"

        try:
            # Parse SQL statement
            parsed = sqlglot.parse_one(sql)
            where_clause = parsed.find(exp.Where)

            if not where_clause:
                return self.values

            # Get WHERE condition expression
            condition_expr = where_clause.this

            # Filter data
            result = []
            for value in self.values:
                if self._evaluate_condition(condition_expr, value):
                    result.append(value)

            return result

        except Exception as e:
            raise ValueError(f"Unable to parse condition statement: {condition}. Error: {str(e)}")

    def _evaluate_condition(self, expr: exp.Expression, value: str) -> bool:
        """
        Evaluate condition expression

        Args:
            expr: sqlglot expression object
            value: Value to evaluate

        Returns:
            Whether the condition is met
        """
        # Handle AND logic
        if isinstance(expr, exp.And):
            conditions = []
            if expr.this:
                conditions.append(expr.this)
            if expr.expression:
                conditions.append(expr.expression)
            return all(self._evaluate_condition(child, value) for child in conditions)

        # Handle OR logic
        if isinstance(expr, exp.Or):
            conditions = []
            if expr.this:
                conditions.append(expr.this)
            if expr.expression:
                conditions.append(expr.expression)
            return any(self._evaluate_condition(child, value) for child in conditions)

        # Handle IN operation
        if isinstance(expr, exp.In):
            column = self._get_column_name(expr.this)
            if column == self.column_name:
                values = self._extract_in_values(expr.args.get("expressions", []))
                return value in values
            return False

        # Handle comparison operators (=, >, <, >=, <=, !=)
        if isinstance(expr, (exp.EQ, exp.GT, exp.LT, exp.GTE, exp.LTE, exp.NEQ)):
            left = expr.this
            right = expr.args.get("expression")

            # Determine which side is column and which side is value
            if isinstance(left, exp.Column):
                column = left.name
                compare_value = self._extract_value(right)
                actual_value = value
            elif isinstance(right, exp.Column):
                column = right.name
                compare_value = self._extract_value(left)
                actual_value = value
                # Reverse comparison direction
                expr = self._reverse_comparison(expr)
            else:
                return False

            if column != self.column_name:
                return False

            # Perform comparison
            return self._compare(actual_value, compare_value, type(expr))

        return False

    def _get_column_name(self, expr: exp.Expression) -> str:
        """Get column name"""
        if isinstance(expr, exp.Column):
            return expr.name
        return ""

    def _extract_value(self, expr: exp.Expression) -> str:
        """Extract literal value"""
        if isinstance(expr, exp.Literal):
            return expr.this
        if isinstance(expr, str):
            return expr
        return str(expr)

    def _extract_in_values(self, expressions: List[exp.Expression]) -> List[str]:
        """Extract value list from IN clause"""
        values = []
        for expr in expressions:
            if isinstance(expr, exp.Literal):
                values.append(expr.this)
            elif isinstance(expr, exp.Tuple):
                # Handle tuple form of IN values
                for item in expr.expressions:
                    if isinstance(item, exp.Literal):
                        values.append(item.this)
        return values

    def _reverse_comparison(self, expr: exp.Expression) -> exp.Expression:
        """Reverse comparison operator (when column is on the right side)"""
        type_map = {
            exp.GT: exp.LT,
            exp.LT: exp.GT,
            exp.GTE: exp.LTE,
            exp.LTE: exp.GTE,
            exp.EQ: exp.EQ,
            exp.NEQ: exp.NEQ,
        }
        return type_map.get(type(expr), type(expr))

    def _compare(self, left: str, right: str, op_type: type) -> bool:
        """Perform comparison operation"""
        if op_type == exp.EQ:
            return left == right
        elif op_type == exp.NEQ:
            return left != right
        elif op_type == exp.GT:
            return left > right
        elif op_type == exp.LT:
            return left < right
        elif op_type == exp.GTE:
            return left >= right
        elif op_type == exp.LTE:
            return left <= right
        return False


def filter_values(values: List[str], condition: str, column_name: str = "partition") -> List[str]:
    """
    Filter data list based on SQL condition statement

    Args:
        values: List of all available values
        condition: SQL WHERE condition statement
        column_name: Column name, default is 'partition'

    Returns:
        List of values that meet the condition

    Examples:
        >>> values = ['20260101', '20260102', '20260103', '20260104']
        >>> filter_values(values, "partition = '20260101'")
        ['20260101']
        >>> filter_values(values, "date IN ('20260101', '20260102')", column_name='date')
        ['20260101', '20260102']
        >>> filter_values(values, "partition >= '20260101' AND partition <= '20260103'")
        ['20260101', '20260102', '20260103']
    """
    filter_obj = PartitionFilter(values, column_name)
    return filter_obj.filter(condition)
