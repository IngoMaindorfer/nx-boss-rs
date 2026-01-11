#!/usr/bin/env python

from curses import meta
import time
import uuid
import collections
from multiprocessing import Value
import copy
import dataclasses
from typing import Annotated, Any
import argparse
import datetime
import json
import pathlib

from fastapi import FastAPI, Query, Form, File, UploadFile
from starlette.middleware.base import BaseHTTPMiddleware
from starlette.requests import Request
import pydantic
import ruamel.yaml
import uvicorn

_DEFAULT_JOB_SETTIGNS = {'continuous_scan':False,'show_message':False,'message':None,'show_thumbnail':False ,'show_scan_button':False,'auto_logout':False,'wait_file_transfer':False,'show_transfer_completion':False,'metadata_setting':None,'job_timeout':0}
_DEFAULT_SCAN_SETTINGS = {'parameters':{'task':{'actions':{'streams':{'sources':{'devControls':{'attributes':[{'attribute':'stop','values':{'value':'true'}},{'attribute':'lap24','values':{'value':24}},{'attribute':'back','values':{'value':'true'}},{'attribute':'initialCount','values':{'value':1}},{'attribute':'decr','values':{'value':'false'}},{'attribute':'ul','values':{'value':'lower'}},{'attribute':'cstep','values':{'value':0}},{'attribute':'xOffset','values':{'value':0}},{'attribute':'yOffset','values':{'value':0}},{'attribute':'font','values':{'value':'horizontalNormal'}},{'attribute':'bold','values':{'value':'false'}},{'attribute':'dirs','values':{'value':'ltor'}},{'attribute':'stringLength','values':{'value':0}},{'attribute':'string','values':{'value':''}}]},'docAnnotations':{'blankPageSkip':{'attributes':[{'attribute':'blankPageSkip','values':{'value':'off'}}]},'thumbnailQuality':{'attributes':[{'attribute':'thumbnailQuality','values':{'value':'1'}}]},'verticalLine':{'attributes':[{'attribute':'verticalLine','values':{'value':'disable'}}]}},'feedControls':{'background':{'attributes':[{'attribute':'bgColor','values':{'value':'black'}}]},'doubleFeed':{'attributes':[{'attribute':'overlap','values':{'value':'disable'}},{'attribute':'length','values':{'value':'disable'}},{'attribute':'response','values':{'value':'notify'}},{'attribute':'deviceSpecification','values':{'value':'disable'}},{'attribute':'iOMFLength','values':{'value':'0'}}]},'ejection':{'attributes':[{'attribute':'synchronousEjection','values':{'value':'disable'}},{'attribute':'synchronousNextFeed','values':{'value':'disable'}}]},'noSeparation':{'attributes':[{'attribute':'noSeparationControl','values':{'value':'deviceSpecification'}}]},'numberOfSheets':{'attributes':[{'attribute':'sheetCounts','values':{'value':'0'}}]},'paperProtection':{'attributes':[{'attribute':'paperProtection','values':{'value':'deviceSpecification'}},{'attribute':'soundJam','values':{'value':'deviceSpecification'}},{'attribute':'paperProtection3','values':{'value':'deviceSpecification'}}]},'prePick':{'attributes':[{'attribute':'prePickControl','values':{'value':'enable'}}]}},'pixelFormats':{'attributes':[{'attribute':'resolution','values':{'value':'300'}},{'attribute':'height','values':{'value':'15552'}},{'attribute':'width','values':{'value':'10624'}},{'attribute':'automaticSize','values':{'value':'enable'}},{'attribute':'offsetWidth','values':{'value':'0'}},{'attribute':'offsetHeight','values':{'value':'0'}},{'attribute':'compression','values':{'value':'jpeg'}},{'attribute':'dropoutColor','values':{'value':'green'}},{'attribute':'jpgSubSampling','values':{'value':'444'}},{'attribute':'endOfPageDetection','values':{'value':'off'}},{'attribute':'overscan','values':{'value':'on'}},{'attribute':'automaticDeskew','values':{'value':'enable'}},{'attribute':'cropMargin','values':{'value':'0.0'}},{'attribute':'tabCropping','values':{'value':'on'}},{'attribute':'jpegQuality','values':{'value':'80'}},{'attribute':'highSpeedMode','values':{'value':'off'}},{'attribute':'paperWidth','values':{'value':'10624'}},{'attribute':'paperLength','values':{'value':'15552'}},{'attribute':'sRGBPattern','values':{'value':'table1'}},{'attribute':'moireRemoval','values':{'value':'deviceSpecification'}}],'pixelFormat':'rgb24'},'readControls':{'imageCacheMode':{'attributes':[{'attribute':'imageCacheMode','values':{'value':'scannerMemory'}}]},'imageTransferMethod':{'attributes':[{'attribute':'imageTransferMethod','values':{'value':'alternate'}}]}},'source':'feeder'}}}}}}


def _now() -> str:
    return datetime.datetime.now().astimezone().isoformat()


def _update_recursive(dest: dict[str, Any], src: dict[str, Any]) -> None:
    for key, value in src.items():
        if isinstance(value, dict):
            _update_recursive(dest[key], value)
            continue
        if key not in dest:
            attrs = [attr for attr in dest.get('attributes', []) if attr['attribute'] == key]
            if attrs:
                _update_recursive(attrs[0]['values'], {'value': value})
                continue
            raise KeyError(f'Unknown attribute {key}')
        default = dest[key]
        if type(value) == type(default):
            dest[key] = value
        elif isinstance(value, int) and isinstance(default, str):
            dest[key] = str(value)
        elif isinstance(value, bool) and isinstance(default, str):
            dest[key] = 'true' if value else 'false'
        else:
            raise ValueError(f'Bad type for attribute {key}: expect {type(default)} but got {type(value)}')


@dataclasses.dataclass(frozen=True)
class Job:
    output_path: str
    job_info: dict[str, Any]
    scan_settings: dict[str, Any]

    @classmethod
    def parse(cls, *, id: int, name: str, job: dict[str, Any]) -> 'Job':
        job_settings = copy.deepcopy(_DEFAULT_JOB_SETTIGNS)
        _update_recursive(job_settings, job.get('job_settings', {}))
        scan_settings = copy.deepcopy(_DEFAULT_SCAN_SETTINGS)
        _update_recursive(
            scan_settings['parameters']['task']['actions']['streams']['sources'],
            job.get('scan_settings', {}),
        )
        output_path = job['output_path']
        if not pathlib.Path(output_path).is_dir():
            raise ValueError(f'output_path {output_path!r} is not a directory')
        return cls(
            output_path=job['output_path'],
            job_info={
                'type': 0,
                'job_id': id,
                'name': name,
                'color': job.get('color', '#4D4D4D'),
                'job_setting': job_settings,
                'hierarchy_list':None,
            },
            scan_settings=scan_settings,
        )


@dataclasses.dataclass(frozen=True)
class Config:
    jobs: list[Job]

    @classmethod
    def parse(cls, config_file: dict[str, Any]) -> 'Config':
        return cls(
            jobs=[
                Job.parse(id=job_id, name=name, job=job)
                for job_id, (name, job) in enumerate(config_file['jobs'].items())
            ],
        )


class BatchMetadata(pydantic.BaseModel):
    job_name: str
    created_at: str
    completed: bool = False
    files: list[dict[str, Any]] = []


class Batch:
    def __init__(self, *, id: str, dir: pathlib.Path, metadata: BatchMetadata):
        self.id = id
        self._dir = dir
        self._metadata = metadata

    @classmethod
    def create(cls, job: Job) -> 'Batch':
        batch_id = uuid.uuid4().hex
        batch_dir = pathlib.Path(job.output_path) / batch_id
        batch_dir.mkdir()
        metadata = BatchMetadata(
            job_name=job.job_info['name'],
            created_at=_now(),
        )
        b = Batch(id=batch_id, dir=batch_dir, metadata=metadata)
        b._dump_metadata()
        return b

    def add_file(self, *, filename: str, content: bytes, parameters: dict[str, Any]):
        file_path = self._dir / filename
        if file_path.name != filename:
            raise ValueError('bad filename')
        file_path.write_bytes(content)
        self._metadata.files.append({
            'filename': filename,
            'received_at': _now(),
            'parameters': parameters,
        })
        self._dump_metadata()

    def complete(self):
        self._metadata.completed = True
        self._dump_metadata()

    def _dump_metadata(self) -> None:
        (self._dir / '.metadata.json').write_text(self._metadata.model_dump_json())
        (self._dir / '.metadata.json').replace(self._dir / 'metadata.json')


class ForceJsonHeaderMiddleware(BaseHTTPMiddleware):
    async def dispatch(self, request: Request, call_next):
        headers = dict(request.scope['headers'])
        if headers.get(b'content-type') == b'application/x-www-form-urlencoded':
            headers[b'content-type'] = b'application/json'
            request.scope['headers'] = list(headers.items())
        return await call_next(request)


app = FastAPI()
app.add_middleware(ForceJsonHeaderMiddleware)


@app.get('/NmWebService/heartbeat')
async def heartbeat():
    return {'system_time': _now()}


class Device(pydantic.BaseModel):
    call_timing: str
    scanner_ip: str
    scanner_mac: str
    scanner_model: str
    scanner_name: str
    scanner_port: str
    scanner_protocol: str
    serial_no: str


@app.post('/NmWebService/device')
async def device(device: Device):
    return {'system_time': _now(), 'server_version': '2.6.0.4'}


@app.get('/NmWebService/authorization')
async def get_authorization(auth_token: str = Query()):
    return {'auth_type': 'none', 'auth_token': ''}


@app.post('/NmWebService/authorization')
async def post_authorization():
    return {
        'access_token':'unused',
        'token_type':'bearer',
        'job_group_name':'nx-boss',
        'job_info': [job.job_info for job in app.state.config.jobs],
    }


@app.get('/NmWebService/scansetting')
async def get_scansetting(job_id: str):
    return app.state.config.jobs[int(job_id)].scan_settings


class BatchRequest(pydantic.BaseModel):
    job_id: str


@app.post('/NmWebService/batch')
async def post_batch(request: BatchRequest):
    job = app.state.config.jobs[int(request.job_id)]
    batches = app.state.batches
    while (first := next(iter(batches.values()), None)) and time.time() - first['last_used'] > 3600:
        batches.popitem(last=False)
    batch = Batch.create(job=job)
    batches[batch.id] = batch
    return {'batch_id': batch.id}


@app.post('/NmWebService/image')
async def post_image(
    image: UploadFile,
    imageparameter: UploadFile,
    parameter: Annotated[bytes, File()],
):
    parameter_decoded = json.loads(parameter.decode())
    batch_id = parameter_decoded['batch_id']
    batch = app.state.batches[batch_id]
    batch.add_file(
        filename=image.filename,
        content=await image.read(),
        parameters=parameter_decoded,
    )


@app.put('/NmWebService/batch/{batch_id}')
async def put_batch(batch_id: str, parameter: Annotated[bytes, File()]):
    batch = app.state.batches.pop(batch_id)
    batch.complete()


@app.delete('/NmWebService/accesstoken')
async def delete_accesstoken():
    return {'CharSet':None,'Parameters':[],'MediaType':'application/json'}


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--host', type=str, default='127.0.0.1')
    parser.add_argument('--port', type=int, default=10447)
    parser.add_argument('--config', '-c', type=str, required=True)
    args = parser.parse_args()

    config_file = ruamel.yaml.YAML().load(pathlib.Path(args.config).read_text())
    app.state.config = Config.parse(config_file)
    app.state.batches = collections.OrderedDict()

    uvicorn.run(app, host=args.host, port=args.port)
