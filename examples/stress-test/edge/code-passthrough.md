---
language: Python
framework: FastAPI
---


# **Python** Code Examples

Here's a Python example using FastAPI:

```python
from fastapi import FastAPI

app = FastAPI()

@app.get("/items/{item_id}")
async def read_item(item_id: int):
    return {"item_id": item_id, "status": "active"}

config = {"key": "value", "nested": {"deep": True}}
template = f"Hello {name}!"
```

After the code block, interpolation resumes: Python with FastAPI.

```json
{
  "name": "test",
  "config": {
    "debug": true
  }
}
```

Final interpolation: **Stack**: `Python`
