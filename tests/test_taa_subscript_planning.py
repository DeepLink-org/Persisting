"""
TAA 轻量访问：tensor[{SESSION: xx}, :, 0:100] 做 address planning。

通过 TensorView(dims) 得到 tensor，用 tensor[..., :, 0:100] 构造 Region，
再用 canonicalize / project_prefix / is_point_query / is_range_query 做 planning。
"""

import pytest

try:
    from persisting.core import (
        Address,
        Dimension,
        Point,
        Range,
        Region,
        TensorView,
        canonicalize,
        is_point_query,
        is_range_query,
        project_prefix,
    )
except ImportError as e:
    pytest.skip(
        f"persisting._core 未安装或不可用，跳过 TAA 测试: {e}",
        allow_module_level=True,
    )


SESSION = Dimension("session", "str")
LAYER = Dimension("layer", "int")
HEAD = Dimension("head", "int")
TIME = Dimension("time", "int")

# 固定维度顺序：(session, layer, head, time)
DIMS = (SESSION, LAYER, HEAD, TIME)
tensor = TensorView(DIMS)


# ---------------------------------------------------------------------------
# 轻量访问：tensor[{SESSION: xx}, :, 0:100]
# ---------------------------------------------------------------------------

class TestTensorViewAccess:
    """tensor[..., :, 0:100] 式访问，得到 Region。"""

    def test_tensor_positional_value_slice(self):
        r = tensor["s1", 0, 2, 0:100]
        assert r[SESSION].value == "s1"
        assert r[LAYER].value == 0
        assert r[HEAD].value == 2
        assert r[TIME].lo == 0 and r[TIME].hi == 100

    def test_tensor_dict_and_slice(self):
        r = tensor[{SESSION: "s1"}, :, :, 0:512]
        assert r[SESSION].value == "s1"
        assert r[TIME].lo == 0 and r[TIME].hi == 512

    def test_tensor_all_slice(self):
        r = tensor[:, :, :, 10:20]
        assert r[TIME].lo == 10 and r[TIME].hi == 20

    def test_tensor_point_query_style(self):
        r = tensor["s1", 0, 2, 100]
        r = canonicalize(r)
        assert is_point_query(r) is True
        key = project_prefix(r, [SESSION, LAYER, HEAD])
        assert tuple(key) == ("s1", 0, 2)

    def test_tensor_range_query_style(self):
        r = tensor["s1", 0, 2, 0:512]
        r = canonicalize(r)
        assert is_point_query(r) is False
        assert is_range_query(r, TIME) is True
        key = project_prefix(r, [SESSION, LAYER, HEAD])
        assert tuple(key) == ("s1", 0, 2)

    def test_tensor_pure_dict(self):
        r = tensor[{SESSION: "s1", TIME: slice(0, 100)}]
        assert r[SESSION].value == "s1"
        assert r[TIME].lo == 0 and r[TIME].hi == 100

    def test_tensor_wrong_length_raises(self):
        with pytest.raises(ValueError, match="下标长度须为"):
            _ = tensor["s1", 0]


# ---------------------------------------------------------------------------
# Planning：canonicalize / project_prefix / is_point_query / is_range_query
# ---------------------------------------------------------------------------

class TestTaaPlanning:
    """用 tensor 下标得到 Region 后做 planning。"""

    def test_planning_point_query(self):
        r = tensor["s1", 0, 2, 100]
        r = canonicalize(r)
        assert is_point_query(r) is True
        assert tuple(project_prefix(r, [SESSION, LAYER, HEAD])) == ("s1", 0, 2)

    def test_planning_range_query(self):
        r = tensor["s1", 0, 2, 0:512]
        r = canonicalize(r)
        assert is_range_query(r, TIME) is True
        assert tuple(project_prefix(r, [SESSION, LAYER, HEAD])) == ("s1", 0, 2)

    def test_planning_from_address(self):
        addr = Address([(SESSION, "s1"), (LAYER, 1), (HEAD, 3), (TIME, 200)])
        prefix = project_prefix(addr, [SESSION, LAYER])
        assert tuple(prefix) == ("s1", 1)

    def test_planning_shift_range(self):
        r = tensor["s1", :, :, 10:20]
        r = r.shift_range(TIME, 5)
        assert r[TIME].lo == 15 and r[TIME].hi == 25
