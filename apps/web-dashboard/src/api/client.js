import axios from 'axios';

// 动态识别 Tauri 生产环境：支持 v1 (tauri:) 和 v2 (tauri.localhost)
const isTauri = typeof window !== 'undefined' && (
  !!window.__TAURI__ || 
  window.location.protocol === 'tauri:' || 
  window.location.hostname === 'tauri.localhost' ||
  window.location.host === 'tauri.localhost'
);

// 生产环境下强制直连 Axum (127.0.0.1:5173)，开发环境下使用 Vite Proxy (/v1)
export const baseURL = isTauri ? 'http://127.0.0.1:5173/v1' : '/v1';
export { isTauri };

const apiClient = axios.create({
  baseURL,
  timeout: 300000, // 延长至 5 分钟，适配核心资产下载
  headers: {
    'Content-Type': 'application/json',
  },
});

// 响应拦截器：统一处理数据解包与错误日志
apiClient.interceptors.response.use(
  (response) => {
    // 如果后端返回结构是 { success: true, data: ... }
    if (response.data && typeof response.data === 'object' && 'success' in response.data) {
      if (response.data.success) {
        // 只有当存在 data 字段时才解包，否则返回整个 response.data
        return 'data' in response.data ? response.data.data : response.data;
      } else {
        return Promise.reject(new Error(response.data.message || '业务逻辑错误'));
      }
    }
    // 否则直接返回 data (适配部分原始 OpenAI 兼容接口)
    return response.data;
  },
  (error) => {
    const message = error.response?.data?.message || error.message || '网络请求失败';
    console.error('🌐 [API Client Error]:', message);
    return Promise.reject(error);
  }
);

export default apiClient;
