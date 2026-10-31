"""LangChain document loader for razorpdf (tested against the local build).

Requires: pip install razorpdf langchain-core
"""

from __future__ import annotations

from typing import Iterator

from langchain_core.document_loaders import BaseLoader
from langchain_core.documents import Document

import razorpdf


class RazorPdfLoader(BaseLoader):
    """One LangChain Document per grounded razorpdf chunk.

    page_content is the chunk Markdown; metadata carries chunk_id,
    page_index, bbox {x0,y0,x1,y1} (PDF points), breadcrumb trail,
    token_estimate, and parsed_by_vlm for citations.
    """

    def __init__(self, pdf_path: str, vlm_url: str | None = None) -> None:
        self.pdf_path = pdf_path
        self.vlm_url = vlm_url

    def lazy_load(self) -> Iterator[Document]:
        doc = razorpdf.parse_file(self.pdf_path, vlm_url=self.vlm_url)
        for c in doc["chunks"]:
            yield Document(
                page_content=c["markdown"],
                metadata={
                    "source": doc["source_path"],
                    "chunk_id": c["chunk_id"],
                    "page_index": c["page_index"],
                    "bbox": c["bbox"],
                    "breadcrumb": c["breadcrumb"],
                    "token_estimate": c["token_estimate"],
                    "parsed_by_vlm": c["parsed_by_vlm"],
                },
            )
