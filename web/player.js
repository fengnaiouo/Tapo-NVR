import {VideoRTC} from '/go2rtc/video-rtc.js';

/** go2rtc 播放器：移除內建控制列，並以事件回報目前的串流模式 */
class TapoVideo extends VideoRTC {
    oninit() {
        super.oninit();
        this.video.controls = false;
        this.video.muted = true;
        this.video.style.objectFit = 'contain';
    }

    report(mode) {
        this.dispatchEvent(new CustomEvent('streammode', {detail: mode}));
    }

    onconnect() {
        const r = super.onconnect();
        if (r) this.report('連線中');
        return r;
    }

    onopen() {
        const r = super.onopen();
        this.onmessage['tapo'] = msg => {
            if (msg.type === 'error') this.report('錯誤');
            else if (['mse', 'hls', 'mp4', 'mjpeg'].includes(msg.type)) this.report(msg.type.toUpperCase());
        };
        return r;
    }

    onclose() {
        // WebRTC 建立後 go2rtc 會關閉訊令用的 WebSocket，屬正常情況
        if (this.pcState === WebSocket.CLOSED) this.report('重新連線');
        return super.onclose();
    }

    onpcvideo(ev) {
        super.onpcvideo(ev);
        if (this.pcState !== WebSocket.CLOSED) this.report('WebRTC');
    }
}

customElements.define('tapo-video', TapoVideo);
