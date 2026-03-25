import apiClient from '../client';

export const monitorService = {
  // 获取流量日志
  getTrafficLogs: (limit = 50) => apiClient.get('/monitor/logs', { params: { limit } }),

  // 删除所有流量日志
  clearTrafficLogs: () => apiClient.delete('/monitor/logs'),

  // 获取系统日志
  getSystemLogs: () => apiClient.get('/logs'),

  // 删除所有系统日志
  clearSystemLogs: () => apiClient.delete('/logs'),
};
