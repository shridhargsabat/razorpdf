# Framework integrations

## LangChain (tested)

`demo/langchain_loader.py` ships a `RazorPdfLoader` — one `Document` per
grounded chunk, with `bbox`, `breadcrumb`, and `chunk_id` in metadata for
citations:

```bash
pip install razorpdf langchain-core
```

```python
from demo.langchain_loader import RazorPdfLoader

docs = RazorPdfLoader("paper.pdf").load()
print(docs[0].page_content[:200])
print(docs[0].metadata["bbox"], docs[0].metadata["breadcrumb"])

# Chunking is already done (layout-aware blocks + tables); index directly:
# from langchain_text_splitters import RecursiveCharacterTextSplitter  # not needed
# from langchain_community.vectorstores import FAISS
# from langchain_openai import OpenAIEmbeddings
# FAISS.from_documents(docs, OpenAIEmbeddings()).save_local("paper_index")
```

## LlamaIndex (snippet, adapt to your version)

```bash
pip install razorpdf llama-index-core
```

```python
import razorpdf
from llama_index.core import Document, VectorStoreIndex

doc = razorpdf.parse_file("paper.pdf")
nodes = [
    Document(
        text=c["markdown"],
        metadata={
            "chunk_id": c["chunk_id"],
            "page_index": c["page_index"],
            "bbox": c["bbox"],
            "breadcrumb": " > ".join(c["breadcrumb"]),
        },
        # stable IDs keep re-index runs idempotent
        id_=f"{doc['source_path']}::{c['chunk_id']}",
    )
    for c in doc["chunks"]
]
index = VectorStoreIndex(nodes)
print(index.as_query_engine().query("What does Table 1 compare?"))
```
