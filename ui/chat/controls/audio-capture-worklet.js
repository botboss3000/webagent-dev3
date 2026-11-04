// AudioWorklet half of composer dictation (audio-capture.js): copies microphone samples off the main thread in 2048-sample blocks, so a busy page does not drop audio. On 'flush' it posts its partial block, then 'flushed'.
class DictationCapture extends AudioWorkletProcessor{
 constructor(){super();this.block=new Float32Array(2048);this.used=0;this.port.onmessage=e=>{if(e.data==='flush'){this.post();this.port.postMessage('flushed');}};}
 // Purpose: Transfer the filled part of the block to the main thread.
 post(){if(!this.used)return;const out=this.block.slice(0,this.used);this.used=0;this.port.postMessage(out,[out.buffer]);}
 // Purpose: Append the first input channel to the block, posting each full block; returning true keeps the node alive.
 process(inputs){const input=inputs[0]?.[0];if(input)for(let i=0;i<input.length;){const n=Math.min(input.length-i,this.block.length-this.used);this.block.set(input.subarray(i,i+n),this.used);this.used+=n;i+=n;if(this.used===this.block.length)this.post();}return true;}
}
registerProcessor('dictation-capture',DictationCapture);
