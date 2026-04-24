import collections
from collections.abc import Awaitable, Callable
from typing import Annotated, Any

import pydantic
from fastapi import FastAPI, File, HTTPException, Query, UploadFile
from starlette.middleware.base import BaseHTTPMiddleware
from starlette.requests import Request
from starlette.responses import Response

from nx_boss.batch import Batch, _now, decode_parameter
from nx_boss.config import Config


class ForceJsonHeaderMiddleware(BaseHTTPMiddleware):
    async def dispatch(
        self,
        request: Request,
        call_next: Callable[[Request], Awaitable[Response]],
    ) -> Response:
        headers = dict(request.scope["headers"])
        if headers.get(b"content-type") == b"application/x-www-form-urlencoded":
            headers[b"content-type"] = b"application/json"
            request.scope["headers"] = list(headers.items())
        return await call_next(request)


def create_app(config: Config) -> FastAPI:
    app = FastAPI()
    app.add_middleware(ForceJsonHeaderMiddleware)

    batches: collections.OrderedDict[str, Batch] = collections.OrderedDict()

    @app.get("/NmWebService/heartbeat")
    async def heartbeat() -> dict[str, Any]:
        return {"system_time": _now()}

    class Device(pydantic.BaseModel):
        call_timing: str
        scanner_ip: str
        scanner_mac: str
        scanner_model: str
        scanner_name: str
        scanner_port: str
        scanner_protocol: str
        serial_no: str

    @app.post("/NmWebService/device")
    async def device(device: Device) -> dict[str, Any]:
        return {"system_time": _now(), "server_version": "2.6.0.4"}

    @app.get("/NmWebService/authorization")
    async def get_authorization(auth_token: str = Query()) -> dict[str, Any]:
        return {"auth_type": "none", "auth_token": ""}

    @app.post("/NmWebService/authorization")
    async def post_authorization() -> dict[str, Any]:
        return {
            "access_token": "unused",
            "token_type": "bearer",
            "job_group_name": "nx-boss",
            "job_info": [job.job_info for job in config.jobs],
        }

    @app.get("/NmWebService/scansetting")
    async def get_scansetting(job_id: str) -> dict[str, Any]:
        return config.jobs[int(job_id)].scan_settings

    class BatchRequest(pydantic.BaseModel):
        job_id: str

    @app.post("/NmWebService/batch")
    async def post_batch(request: BatchRequest) -> dict[str, Any]:
        job = config.jobs[int(request.job_id)]
        batch = Batch.create(job=job)
        batches[batch.id] = batch
        return {"batch_id": batch.id}

    @app.post("/NmWebService/image")
    async def post_image(
        image: UploadFile,
        imageparameter: UploadFile,
        parameter: Annotated[bytes, File()],
    ) -> None:
        parameters = decode_parameter(parameter)
        batch_id = parameters["batch_id"]
        batch = batches[batch_id]
        try:
            batch.add_file(
                filename=image.filename or "image",
                content=await image.read(),
                parameters=parameters,
            )
        except ValueError as e:
            raise HTTPException(status_code=400, detail=str(e)) from e

    @app.put("/NmWebService/batch/{batch_id}")
    async def put_batch(batch_id: str, parameter: Annotated[bytes, File()]) -> None:
        batch = batches.pop(batch_id)
        batch.complete()

    @app.delete("/NmWebService/accesstoken")
    async def delete_accesstoken() -> dict[str, Any]:
        return {"CharSet": None, "Parameters": [], "MediaType": "application/json"}

    return app
