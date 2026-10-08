#!/usr/bin/env python3
"""真实测试 MCP 工具：等待 FIFO 后返回结果，同时记录服务端开始、取消通知与完成。"""
import json
from pathlib import Path
import sys
import threading

lock=threading.Lock()
root=Path('/sandbox/project')

def handle(message):
    with lock:
        with (root/'mcp-wire.jsonl').open('a') as log: log.write(json.dumps(message)+'\n')
    method=message.get('method')
    if method=='notifications/cancelled':
        (root/'mcp-cancelled').write_text(json.dumps(message))
    if 'id' not in message: return
    if method=='server/discover' and (root/'task-mode').exists():
        result={'supportedVersions':['2026-07-28'],'capabilities':{'tools':{},'extensions':{'io.modelcontextprotocol/tasks':{}}}}
    elif method=='initialize':
        (root/'mcp-init.json').write_text(json.dumps(message))
        result={'protocolVersion':message['params']['protocolVersion'],'capabilities':{'tools':{}},'serverInfo':{'name':'fifo','version':'1'}}
    elif method=='tools/list':
        result={'tools':[{'name':'wait','description':'Wait for the local FIFO','inputSchema':{'type':'object','properties':{}},'execution':{'taskSupport':'optional'}}]}
    elif method=='tools/call':
        (root/'mcp-request.json').write_text(json.dumps(message))
        (root/'mcp-started').touch()
        def finish():
            with open('/sandbox/fifos/mcp') as fifo: fifo.readline()
            (root/'mcp-finished').touch()
        extensions=message.get('params',{}).get('_meta',{}).get('io.modelcontextprotocol/clientCapabilities',{}).get('extensions',{})
        if (root/'task-mode').exists() and 'io.modelcontextprotocol/tasks' in extensions:
            threading.Thread(target=finish,daemon=True).start()
            result={'resultType':'task','taskId':'fifo-task','status':'working','pollIntervalMs':250,'createdAt':'2026-10-08T00:00:00Z','lastUpdatedAt':'2026-10-08T00:00:00Z','ttlMs':None}
        else:
            finish()
            result={'content':[{'type':'text','text':'MCP_FINISHED'}]}
    elif method=='tasks/get':
        (root/'task-polled').touch()
        result={'taskId':'fifo-task','status':'working','pollIntervalMs':250,'createdAt':'2026-10-08T00:00:00Z','lastUpdatedAt':'2026-10-08T00:00:00Z','ttlMs':None}
        if (root/'mcp-finished').exists():
            result.update(status='completed',result={'content':[{'type':'text','text':'MCP_FINISHED'}]})
    elif method=='tasks/cancel':
        (root/'mcp-cancelled').write_text(json.dumps(message))
        result={'taskId':'fifo-task','status':'cancelled'}
    else: result={}
    if (root/'task-mode').exists():
        result.setdefault('resultType','complete')
        if method=='tools/list': result.update(ttlMs=0,cacheScope='private')
        if 'result' in result: result['result'].setdefault('resultType','complete')
    with lock:
        print(json.dumps({'jsonrpc':'2.0','id':message['id'],'result':result}),flush=True)

for line in sys.stdin:
    threading.Thread(target=handle,args=(json.loads(line),),daemon=True).start()
